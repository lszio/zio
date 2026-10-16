//! Native storage and capability mechanisms for the source-defined interpreter.
//! Only the embedded implementation sources cross the Rust reader/evaluator.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Weak};

use im::{HashMap, Vector, vector};

use crate::context::{EvalContext, EvalEngine, EvalRuntime, ModuleRegistry};
use crate::env::Env;
use crate::error::{EvalError, ReaderError, SpanContext};
use crate::module::Module;
use crate::sexp::Sexp;
use crate::span::{BytePos, SourceId, SourceMap, Span};
use crate::special::TailResult;
use crate::syntax::SyntaxNode;
use crate::value::{Function, Macro, NativeFn, Value};

const SOURCES: [(&str, &str); 3] = [
    (
        "<zio-interpreter/reader>",
        include_str!("../assets/interpreter/reader.zio"),
    ),
    (
        "<zio-interpreter/expand>",
        include_str!("../assets/interpreter/expand.zio"),
    ),
    (
        "<zio-interpreter/evaluator>",
        include_str!("../assets/interpreter/evaluator.zio"),
    ),
];

struct State {
    implementation: Arc<Env>,
    installed: Cell<bool>,
    installing: Cell<bool>,
    raw_reader: Cell<bool>,
    source_ids: RefCell<HashSet<SourceId>>,
    // ponytail: handles grow with created scopes; use traced handles if persistent-session memory becomes limiting.
    environments: RefCell<Vec<Weak<Env>>>,
    environment_ids: RefCell<std::collections::HashMap<usize, usize>>,
    intrinsic_ids: HashSet<usize>,
    held_environments: RefCell<Vec<Arc<Env>>>,
    errors: RefCell<Vec<Option<EvalError>>>,
    active: Cell<usize>,
    loader_scope: Rc<Cell<usize>>,
}

impl State {
    fn retain_env(&self, env: &Arc<Env>) -> Value {
        let identity = Arc::as_ptr(env) as usize;
        if let Some(index) = self.environment_ids.borrow().get(&identity) {
            return Value::Integer(*index as i64);
        }
        let mut environments = self.environments.borrow_mut();
        environments.push(Arc::downgrade(env));
        let index = environments.len();
        self.environment_ids.borrow_mut().insert(identity, index);
        self.held_environments.borrow_mut().push(env.clone());
        Value::Integer(index as i64)
    }

    fn env(&self, handle: &Value, engine: &dyn EvalEngine) -> Result<Arc<Env>, EvalError> {
        let handle = integer(handle)?;
        if handle == 0 {
            return Ok(engine.env().clone());
        }
        self.environments
            .borrow()
            .get(handle - 1)
            .and_then(Weak::upgrade)
            .ok_or_else(|| EvalError::invalid_form("invalid interpreter environment handle"))
    }

    fn install(&self, engine: &dyn EvalEngine) -> Result<(), EvalError> {
        if self.installed.get() {
            return Ok(());
        }
        if self.installing.replace(true) {
            return Err(EvalError::invalid_form(
                "recursive interpreter installation",
            ));
        }
        let reset = RestoreBool(&self.installing, false);
        crate::builtins::setup_env(&self.implementation);
        crate::syntax::register(&self.implementation);
        self.install_source(engine, "<zio-interpreter/core>", crate::stdlib_source())?;
        for (name, source) in SOURCES {
            self.install_source(engine, name, source)?;
        }
        for name in [
            "zi-read",
            "zi-eval",
            "zi-apply",
            "zi-expand",
            "zi-expand-data",
        ] {
            if !matches!(
                self.implementation.get_owned(name),
                Some(Value::Function(_))
            ) {
                return Err(EvalError::invalid_form(format!(
                    "interpreter implementation missing {name}"
                )));
            }
        }
        self.installed.set(true);
        drop(reset);
        Ok(())
    }

    fn install_source(
        &self,
        engine: &dyn EvalEngine,
        name: &str,
        source: &str,
    ) -> Result<(), EvalError> {
        let id = engine.source_map().register(name.into(), source.into());
        self.source_ids.borrow_mut().insert(id);
        let forms = crate::reader::reader::read_program_with_source(source, id)
            .map_err(|error| error.into_eval(name))?;
        let implementation = RootEngine {
            underlying: engine,
            root: &self.implementation,
        };
        for form in forms {
            crate::eval::eval(&form, &implementation)?;
        }
        Ok(())
    }

    fn implementation_call(
        &self,
        name: &str,
        args: Vector<Value>,
        engine: &dyn EvalEngine,
    ) -> Result<Value, EvalError> {
        self.install(engine)?;
        let function = self
            .implementation
            .get_owned(name)
            .ok_or_else(|| EvalError::symbol_not_found(name))?;
        finish(crate::eval::apply(function, args, engine)?, engine)
    }

    fn envelope(
        &self,
        result: Result<Value, EvalError>,
        fallback: Option<Span>,
        engine: &dyn EvalEngine,
    ) -> Value {
        match result {
            Ok(value) => map([("ok", Value::Boolean(true)), ("value", value)]),
            Err(mut error) => {
                let context = error_context_mut(&mut error);
                if context.span.is_some_and(|span| {
                    // Implementation sources only, by exact registration:
                    // the private interpreter sources and the compiler
                    // bundle (interpreted bootstrap parts or an installed
                    // compiled artifact's provenance). A user file that
                    // merely shares a bundle filename keeps its spans, as
                    // does a user-defined macro body failing mid-expansion.
                    self.source_ids.borrow().contains(&span.source_id)
                        || crate::bytecode::is_compiler_bundle_source(
                            engine.source_map(),
                            span.source_id,
                        )
                }) {
                    context.span = None;
                }
                error = error.with_fallback_span(fallback);
                let kind = crate::special::zos_forms::extract_error_type(&error);
                let message = error.to_string();
                let mut errors = self.errors.borrow_mut();
                let handle = errors.len();
                errors.push(Some(error));
                map([
                    ("ok", Value::Boolean(false)),
                    ("error", Value::Integer(handle as i64)),
                    ("type", keyword(kind)),
                    ("message", Value::String(message)),
                ])
            }
        }
    }
}

struct RestoreBool<'a>(&'a Cell<bool>, bool);
impl Drop for RestoreBool<'_> {
    fn drop(&mut self) {
        self.0.set(self.1);
    }
}

struct Entry<'a>(&'a State);
impl<'a> Entry<'a> {
    fn enter(state: &'a State) -> Self {
        state.active.set(state.active.get() + 1);
        state.loader_scope.set(state.loader_scope.get() + 1);
        Self(state)
    }
}
impl Drop for Entry<'_> {
    fn drop(&mut self) {
        self.0.loader_scope.set(self.0.loader_scope.get() - 1);
        self.0.active.set(self.0.active.get() - 1);
        if self.0.active.get() == 0 {
            self.0.held_environments.borrow_mut().clear();
            for error in self.0.errors.borrow_mut().iter_mut() {
                *error = None;
            }
        }
    }
}

/// Register lazy public entries and storage/capability primitives in this context.
pub fn register(ctx: &EvalContext) {
    if matches!(&ctx.env.get_owned("zio-meta-eval"), Some(Value::NativeFunction(f)) if f.name() == "zio-meta-eval")
    {
        return;
    }
    register_with_scope(ctx, Rc::new(Cell::new(0)));
}

pub(crate) fn register_with_scope(ctx: &EvalContext, loader_scope: Rc<Cell<usize>>) {
    let state = Rc::new(State {
        implementation: Arc::new(Env::new(None)),
        installed: Cell::new(false),
        installing: Cell::new(false),
        raw_reader: Cell::new(false),
        source_ids: RefCell::new(HashSet::new()),
        environments: RefCell::new(Vec::new()),
        environment_ids: RefCell::new(std::collections::HashMap::new()),
        intrinsic_ids: [
            "eval",
            "macroexpand",
            "macroexpand-1",
            "read-syntax",
            "read-string",
            "load",
            "apply",
            "map",
            "filter",
            "reduce",
            "sort",
            "future-call",
        ]
        .into_iter()
        .filter_map(|name| match ctx.env.get(name) {
            Some(Value::NativeFunction(function)) => Some(function.id()),
            _ => None,
        })
        .collect(),
        held_environments: RefCell::new(Vec::new()),
        errors: RefCell::new(Vec::new()),
        active: Cell::new(0),
        loader_scope,
    });
    for name in [
        "chars",
        "byte-count",
        "scalar",
        "reader-error",
        "env",
        "callable-info",
        "function",
        "macro",
        "call",
        "protect",
        "reader-protect",
        "raise",
        "error",
        "module",
        "zos",
        "source",
        "register-source",
        "future-info",
        "vector-swap",
    ] {
        let weak = Rc::downgrade(&state);
        let full_name = format!("interpreter/{name}");
        let function =
            Value::NativeFunction(NativeFn::new(full_name.clone(), move |args, engine| {
                let state = weak
                    .upgrade()
                    .ok_or_else(|| EvalError::invalid_form("expired interpreter context"))?;
                mechanism(&state, name, args, engine)
            }));
        ctx.env.set(full_name.clone(), function.clone());
        state.implementation.set(full_name, function);
    }
    for (name, evaluate) in [("zio-meta-read", false), ("zio-meta-eval", true)] {
        let state = state.clone();
        ctx.env.set(
            name.into(),
            Value::NativeFunction(NativeFn::new(name, move |args, engine| {
                arity(&args, 2)?;
                let source = string(&args[0])?;
                let name = string(&args[1])?;
                let _entry = Entry::enter(&state);
                state.install(engine)?;
                let id = engine.source_map().register(name.into(), source.into());
                let nodes = state.implementation_call(
                    "zi-read",
                    vector![Value::String(source.into()), Value::Integer(id.0 as i64)],
                    engine,
                )?;
                validate_nodes(&nodes, engine)?;
                if evaluate {
                    state.implementation_call("zi-eval", vector![nodes, Value::Integer(0)], engine)
                } else {
                    Ok(nodes)
                }
            })),
        );
    }
}

/// Execute source through the source-defined reader and evaluator, never Rust target eval.
pub fn eval_source(ctx: &EvalContext, name: &str, source: &str) -> Result<Value, EvalError> {
    public_entry(ctx, "zio-meta-eval", name, source)
}

/// Read source through the source-defined lexer/reader into strict syntax nodes.
pub fn read_source(ctx: &EvalContext, name: &str, source: &str) -> Result<Value, EvalError> {
    public_entry(ctx, "zio-meta-read", name, source)
}

fn public_entry(
    ctx: &EvalContext,
    entry: &str,
    name: &str,
    source: &str,
) -> Result<Value, EvalError> {
    register(ctx);
    let function = ctx
        .env
        .get_owned(entry)
        .ok_or_else(|| EvalError::symbol_not_found(entry))?;
    finish(
        crate::eval::apply(
            function,
            vector![Value::String(source.into()), Value::String(name.into())],
            ctx,
        )?,
        ctx,
    )
}

fn mechanism(
    state: &State,
    name: &str,
    args: Vector<Value>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    match name {
        "chars" => {
            arity(&args, 1)?;
            let mut line = 1;
            let mut col = 1;
            let mut output = Vector::new();
            for (byte, scalar) in string(&args[0])?.char_indices() {
                output.push_back(map([
                    ("text", Value::String(scalar.to_string())),
                    ("byte", Value::Integer(byte as i64)),
                    ("line", Value::Integer(line)),
                    ("col", Value::Integer(col)),
                    ("space", Value::Boolean(scalar.is_whitespace())),
                ]));
                if scalar == '\n' {
                    line += 1;
                    col = 1;
                } else {
                    col += scalar.len_utf8() as i64;
                }
            }
            Ok(Value::Vector(output))
        }
        "byte-count" => {
            arity(&args, 1)?;
            Ok(Value::Integer(string(&args[0])?.len() as i64))
        }
        "scalar" => {
            arity(&args, 2)?;
            scalar(keyword_text(&args[0])?, string(&args[1])?)
        }
        "register-source" => {
            arity(&args, 2)?;
            Ok(Value::Integer(
                engine
                    .source_map()
                    .register(string(&args[1])?.into(), string(&args[0])?.into())
                    .0 as i64,
            ))
        }
        "reader-error" => {
            arity(&args, 3)?;
            let error = match keyword_text(&args[0])? {
                "eof" => ReaderError::UnexpectedEOF,
                "token" => ReaderError::UnexpectedToken(string(&args[1])?.into()),
                "odd-map" => ReaderError::OddMapElements,
                "quote" => ReaderError::MissingQuoteExpr,
                "string" => ReaderError::MalformedString(string(&args[1])?.into()),
                _ => {
                    return Err(EvalError::invalid_form(
                        "unknown interpreter reader error kind",
                    ));
                }
            };
            let span = span(&args[2], engine)?;
            let name = span
                .map(|span| engine.source_map().source_name(span.source_id))
                .unwrap_or_else(|| "<source>".into());
            let error = match span {
                Some(span) => error.at(span),
                None => error,
            };
            Err(if state.raw_reader.get() {
                EvalError::custom(error.to_string())
            } else {
                error.into_eval(&name)
            })
        }
        "env" => environment(state, &args, engine),
        "callable-info" => {
            arity(&args, 1)?;
            callable_info(state, &args[0], engine)
        }
        "future-info" => {
            arity(&args, 1)?;
            let function = match &args[0] {
                Value::Function(function) => function,
                other => return Err(EvalError::type_error("function", other.value_type())),
            };
            let body = SyntaxNode::from_sexp(&function.body);
            validate_node(&body, engine)?;
            Ok(map([
                (
                    "params",
                    Value::Vector(function.params.iter().cloned().map(Value::String).collect()),
                ),
                ("env", state.retain_env(&function.env)),
                ("body", Value::Vector(vector![body.to_value()])),
            ]))
        }
        "vector-swap" => {
            arity(&args, 3)?;
            let input = vector_value(&args[0])?;
            let left = integer(&args[1])?;
            let right = integer(&args[2])?;
            let left_value = input
                .get(left)
                .ok_or_else(|| EvalError::index_out_of_bounds(left, input.len()))?
                .clone();
            let right_value = input
                .get(right)
                .ok_or_else(|| EvalError::index_out_of_bounds(right, input.len()))?
                .clone();
            let mut output = input.clone();
            if left != right {
                output[left] = right_value;
                output[right] = left_value;
            }
            Ok(Value::Vector(output))
        }
        "function" | "macro" => {
            let offset = usize::from(name == "macro");
            arity(&args, 4 + offset)?;
            let params = strings(&args[offset])?;
            let rest_param = optional_string(&args[offset + 1])?;
            let body = stored_body(&args[offset + 2], engine)?;
            let env = state.env(&args[offset + 3], engine)?;
            if offset == 0 {
                Ok(Value::Function(Arc::new(Function {
                    params,
                    rest_param,
                    body,
                    env,
                    compiled: None,
                })))
            } else {
                Ok(Value::Macro(Arc::new(Macro {
                    name: string(&args[0])?.into(),
                    params,
                    rest_param,
                    body,
                    env,
                    compiled: None,
                })))
            }
        }
        "call" => {
            arity(&args, 4)?;
            let env = state.env(&args[2], engine)?;
            let fallback = span(&args[3], engine)?;
            let meta = MetaEngine {
                underlying: engine,
                state,
                root: env,
            };
            let result = target_call(
                state,
                args[0].clone(),
                vector_value(&args[1])?.clone(),
                &meta,
            );
            Ok(state.envelope(result, fallback, engine))
        }
        "protect" | "reader-protect" => {
            arity(&args, 3)?;
            let fallback = span(&args[2], engine)?;
            let function = match &args[0] {
                Value::Function(function)
                    if Arc::ptr_eq(&function.env, &state.implementation)
                        || crate::bytecode::is_compiler_bundle_function(
                            function,
                            engine.source_map(),
                        ) =>
                {
                    args[0].clone()
                }
                _ => {
                    return Err(EvalError::invalid_form(
                        "protect requires an interpreter implementation function",
                    ));
                }
            };
            let _raw_reader = if name == "reader-protect" {
                Some(RestoreBool(
                    &state.raw_reader,
                    state.raw_reader.replace(true),
                ))
            } else {
                None
            };
            let result = crate::eval::apply(function, vector_value(&args[1])?.clone(), engine)
                .and_then(|result| finish(result, engine));
            Ok(state.envelope(result, fallback, engine))
        }
        "raise" => {
            arity(&args, 1)?;
            let handle = integer(&args[0])?;
            let error = state
                .errors
                .borrow_mut()
                .get_mut(handle)
                .and_then(Option::take)
                .ok_or_else(|| EvalError::invalid_form("invalid interpreter error handle"))?;
            Err(error)
        }
        "error" => {
            arity(&args, 3)?;
            Err(typed_error(&args[0], &args[1])?.with_opt_span(span(&args[2], engine)?))
        }
        "module" => module(state, &args, engine),
        "zos" => zos(state, &args, engine),
        "source" => {
            arity(&args, 1)?;
            let path = PathBuf::from(string(&args[0])?);
            let path = if path.is_absolute() {
                path
            } else if let Some(base) = engine.source_dir() {
                let candidate = base.join(&path);
                if engine.io().file_exists(&candidate.to_string_lossy())? {
                    candidate
                } else {
                    PathBuf::from(engine.io().current_dir()?).join(path)
                }
            } else {
                PathBuf::from(engine.io().current_dir()?).join(path)
            };
            let source = engine.io().read_file(&path.to_string_lossy())?;
            let id = engine
                .source_map()
                .register(path.to_string_lossy().into_owned(), source.clone());
            Ok(map([
                ("source", Value::String(source)),
                ("source-id", Value::Integer(id.0 as i64)),
            ]))
        }
        _ => Err(EvalError::invalid_form("unknown interpreter mechanism")),
    }
}

fn environment(
    state: &State,
    args: &Vector<Value>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    minimum(args, 2)?;
    let op = keyword_text(&args[0])?;
    if op == "root" {
        arity(args, 2)?;
        return Ok(Value::Integer(0));
    }
    if op == "new" {
        arity(args, 2)?;
        let outer = if args[1] == Value::Nil {
            None
        } else {
            Some(state.env(&args[1], engine)?)
        };
        return Ok(state.retain_env(&Arc::new(Env::new(outer))));
    }
    let env = state.env(&args[1], engine)?;
    match op {
        "get" => {
            arity(args, 3)?;
            let name = string(&args[2])?;
            env.get(name)
                .or_else(|| crate::special::module_forms::resolve_qualified(engine, name))
                .ok_or_else(|| EvalError::symbol_not_found(name))
        }
        "owned" => {
            arity(args, 3)?;
            let value = env.get_owned(string(&args[2])?);
            Ok(Value::Vector(vector![
                Value::Boolean(value.is_some()),
                value.unwrap_or(Value::Nil)
            ]))
        }
        "lookup" => {
            arity(args, 3)?;
            let name = string(&args[2])?;
            let value = env
                .get(name)
                .or_else(|| crate::special::module_forms::resolve_qualified(engine, name));
            Ok(Value::Vector(vector![
                Value::Boolean(value.is_some()),
                value.unwrap_or(Value::Nil)
            ]))
        }
        "set" | "update" => {
            arity(args, 4)?;
            let name = string(&args[2])?;
            if op == "set" || !env.set_global(name, args[3].clone()) {
                env.set(name.into(), args[3].clone());
            }
            Ok(args[3].clone())
        }
        _ => Err(EvalError::invalid_form(
            "unknown interpreter environment operation",
        )),
    }
}

fn callable_info(
    state: &State,
    value: &Value,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    if let Value::Macro(mac) = value {
        if let Some(compiled) = &mac.compiled {
            let callable = Value::Function(Arc::new(Function {
                params: mac.params.clone(),
                rest_param: mac.rest_param.clone(),
                body: mac.body.clone(),
                env: mac.env.clone(),
                compiled: Some(compiled.clone()),
            }));
            let body = if matches!(mac.body, Sexp::Nil) {
                Vector::new()
            } else {
                let node = SyntaxNode::from_sexp(&mac.body);
                validate_node(&node, engine)?;
                vector![node.to_value()]
            };
            return Ok(map([
                ("kind", keyword("macro")),
                ("name", Value::String(mac.name.clone())),
                (
                    "params",
                    Value::Vector(mac.params.iter().cloned().map(Value::String).collect()),
                ),
                (
                    "rest",
                    mac.rest_param.clone().map_or(Value::Nil, Value::String),
                ),
                ("env", state.retain_env(&mac.env)),
                ("body", Value::Vector(body)),
                ("compiled-callable", callable),
            ]));
        }
    }
    let (kind, name, params, rest, body, env) = match value {
        Value::Function(function) if function.compiled.is_none() => (
            "function",
            "",
            &function.params,
            &function.rest_param,
            &function.body,
            &function.env,
        ),
        Value::Macro(mac) if mac.compiled.is_none() => (
            "macro",
            mac.name.as_str(),
            &mac.params,
            &mac.rest_param,
            &mac.body,
            &mac.env,
        ),
        Value::NativeFunction(function) => {
            return Ok(map([
                ("kind", keyword("native")),
                ("name", Value::String(function.name().into())),
                ("callable", value.clone()),
                (
                    "intrinsic",
                    Value::Boolean(state.intrinsic_ids.contains(&function.id())),
                ),
            ]));
        }
        _ => return Ok(Value::Nil),
    };
    let syntax = SyntaxNode::from_sexp(body);
    validate_node(&syntax, engine)?;
    Ok(map([
        ("kind", keyword(kind)),
        ("name", Value::String(name.into())),
        (
            "params",
            Value::Vector(params.iter().cloned().map(Value::String).collect()),
        ),
        ("rest", rest.clone().map_or(Value::Nil, Value::String)),
        ("body", Value::Vector(vector![syntax.to_value()])),
        ("env", state.retain_env(env)),
    ]))
}

fn target_call(
    state: &State,
    callable: Value,
    args: Vector<Value>,
    engine: &MetaEngine<'_>,
) -> Result<Value, EvalError> {
    if matches!(callable, Value::Macro(_)) {
        return Err(EvalError::invalid_form(
            "target macro must be expanded by zi-expand",
        ));
    }
    if let Value::NativeFunction(function) = &callable {
        if state.intrinsic_ids.contains(&function.id()) {
            let descriptor = callable_info(state, &callable, engine.underlying)?;
            return state.implementation_call(
                "zi-apply",
                vector![descriptor, Value::Vector(args)],
                engine.underlying,
            );
        }
    }
    finish(crate::eval::apply(callable, args, engine)?, engine)
}

fn stored_body(value: &Value, engine: &dyn EvalEngine) -> Result<Sexp, EvalError> {
    let nodes = validate_nodes(value, engine)?;
    if nodes.is_empty() {
        return Ok(Sexp::Nil);
    }
    if nodes.len() == 1 {
        return nodes[0].to_sexp();
    }
    let mut expressions = Vector::new();
    expressions.push_back(Sexp::Symbol("do".into(), None));
    for node in nodes {
        expressions.push_back(node.to_sexp()?);
    }
    Ok(Sexp::List(expressions, None))
}

fn validate_nodes(value: &Value, engine: &dyn EvalEngine) -> Result<Vec<SyntaxNode>, EvalError> {
    vector_value(value)?
        .iter()
        .map(|value| {
            let node = SyntaxNode::from_value(value)
                .ok_or_else(|| EvalError::invalid_form("malformed syntax node"))?;
            validate_node(&node, engine)?;
            Ok(node)
        })
        .collect()
}

fn validate_node(node: &SyntaxNode, engine: &dyn EvalEngine) -> Result<(), EvalError> {
    node.validate()?;
    validate_locations(node, engine)
}

fn validate_locations(node: &SyntaxNode, engine: &dyn EvalEngine) -> Result<(), EvalError> {
    if let Some(location) = node.span {
        validate_span(location, engine)?;
    }
    if let crate::syntax::Origin::MacroCall {
        call_site: Some(location),
        ..
    } = &node.origin
    {
        validate_span(*location, engine)?;
    }
    for child in &node.children {
        validate_locations(child, engine)?;
    }
    Ok(())
}

fn span(value: &Value, engine: &dyn EvalEngine) -> Result<Option<Span>, EvalError> {
    if value == &Value::Nil {
        return Ok(None);
    }
    let fields = match value {
        Value::Map(fields) if fields.len() == 5 => fields,
        _ => return Err(EvalError::invalid_form("malformed interpreter span")),
    };
    let field = |name| {
        integer(
            fields
                .get(&keyword(name))
                .ok_or_else(|| EvalError::invalid_form("malformed interpreter span"))?,
        )
    };
    let location = Span::new(
        SourceId(field("source")?),
        BytePos(field("start")?),
        BytePos(field("end")?),
        field("line")?,
        field("col")?,
    );
    validate_span(location, engine)?;
    Ok(Some(location))
}

fn validate_span(span: Span, engine: &dyn EvalEngine) -> Result<(), EvalError> {
    if span.start > span.end || span.line == 0 || span.col == 0 {
        return Err(EvalError::invalid_form("malformed interpreter span"));
    }
    if span.source_id == SourceId::NONE {
        return Ok(());
    }
    if !engine.source_map().valid_span(span) {
        return Err(EvalError::invalid_form("malformed interpreter span"));
    }
    Ok(())
}

fn scalar(kind: &str, text: &str) -> Result<Value, EvalError> {
    Ok(match kind {
        "integer" => text.parse::<i64>().map_or(Value::Nil, Value::Integer),
        "float" => text.parse::<f64>().map_or(Value::Nil, Value::Float),
        "integer-text" => text
            .parse::<i64>()
            .map_or(Value::Nil, |value| Value::String(value.to_string())),
        "float-text" => text
            .parse::<f64>()
            .map_or(Value::Nil, |value| Value::String(value.to_string())),
        "string-debug" => Value::String(format!("{text:?}")),
        "symbol" if !text.is_empty() => Value::Symbol(text.into()),
        "keyword" => Value::Keyword(text.into()),
        "string" => Value::String(text.into()),
        "nil" if text.is_empty() || text == "nil" => Value::Nil,
        "boolean" if text == "true" => Value::Boolean(true),
        "boolean" if text == "false" => Value::Boolean(false),
        "char" | "char-code" if text.chars().count() == 1 => {
            let character = text.chars().next().expect("one scalar");
            if kind == "char" {
                Value::Char(character)
            } else {
                Value::Integer(character as i64)
            }
        }
        _ => return Err(EvalError::invalid_form("invalid interpreter scalar")),
    })
}

fn typed_error(kind: &Value, payload: &Value) -> Result<EvalError, EvalError> {
    let error = match keyword_text(kind)? {
        "symbol" => EvalError::symbol_not_found(string(payload)?),
        "invalid-form" => EvalError::invalid_form(string(payload)?),
        "macro" => EvalError::macro_error(string(payload)?),
        "custom" => EvalError::custom(string(payload)?),
        "not-function" => EvalError::not_a_function(string(payload)?),
        "recur-not-tail" => EvalError::recur_not_tail(),
        "recur-without-loop" => EvalError::recur_without_loop(),
        "arity" | "min-arity" | "range-arity" => {
            let values = vector_value(payload)?;
            match keyword_text(kind)? {
                "arity" => {
                    arity(values, 2)?;
                    EvalError::wrong_arg_count(integer(&values[0])?, integer(&values[1])?)
                }
                "min-arity" => {
                    arity(values, 2)?;
                    EvalError::wrong_arg_count_min(integer(&values[0])?, integer(&values[1])?)
                }
                _ => {
                    arity(values, 3)?;
                    EvalError::wrong_arg_count_range(
                        integer(&values[0])?,
                        integer(&values[1])?,
                        integer(&values[2])?,
                    )
                }
            }
        }
        "type" => {
            let values = vector_value(payload)?;
            arity(values, 2)?;
            let expected = match string(&values[0])? {
                "string" => "string",
                "symbol" => "symbol",
                "keyword" => "keyword",
                "number" => "number",
                "integer" => "integer",
                "boolean" => "boolean",
                "character" => "character",
                "list" => "list",
                "vector" => "vector",
                "map" => "map",
                "function" => "function",
                "callable" => "callable",
                "list or vector" => "list or vector",
                "keyword or symbol" => "keyword or symbol",
                "GenericFunction" => "GenericFunction",
                "class" => "class",
                "syntax node" => "syntax node",
                "sequence" => "sequence",
                "sequential" => "sequential",
                _ => {
                    return Err(EvalError::invalid_form(
                        "unsupported interpreter type expectation",
                    ));
                }
            };
            EvalError::type_error(expected, string(&values[1])?)
        }
        _ => return Err(EvalError::invalid_form("unknown interpreter error kind")),
    };
    Ok(error)
}

fn module(
    state: &State,
    args: &Vector<Value>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    minimum(args, 2)?;
    let env = state.env(&args[1], engine)?;
    match keyword_text(&args[0])? {
        "push-exports" => {
            arity(args, 2)?;
            engine.push_module_exports();
            Ok(Value::Nil)
        }
        "add-export" => {
            arity(args, 3)?;
            engine.add_module_export(string(&args[2])?.into());
            Ok(Value::Nil)
        }
        "take-exports" => {
            arity(args, 2)?;
            Ok(Value::Vector(
                engine
                    .take_module_exports()
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ))
        }
        "register" => {
            arity(args, 5)?;
            let name = module_name(string(&args[2])?)?;
            let exports = strings(&args[3])?.into_iter().collect();
            let source = if args[4] == Value::Nil {
                None
            } else {
                let id = SourceId(integer(&args[4])?);
                if engine.source_map().get(id).is_none() {
                    return Err(EvalError::invalid_form("unregistered module source"));
                }
                Some(id)
            };
            let module = Module {
                name,
                env,
                exports,
                source,
            };
            module.validate_exports()?;
            engine.register_module(module);
            Ok(Value::Nil)
        }
        "require" => {
            arity(args, 3)?;
            let name = module_name(string(&args[2])?)?;
            let module = if let Some(module) = engine.find_module(&name) {
                module
            } else {
                let loaded = engine.call_loader(&name, "", &env).ok_or_else(|| {
                    EvalError::custom(format!(
                        "module not found: {} (no module loader configured)",
                        name.join(".")
                    ))
                })??;
                loaded.validate_exports()?;
                engine.register_module(loaded.clone());
                loaded
            };
            module.validate_exports()?;
            let mut exports = HashMap::new();
            for name in &module.exports {
                exports.insert(
                    Value::String(name.clone()),
                    module
                        .exported(name)
                        .ok_or_else(|| EvalError::symbol_not_found(name))?,
                );
            }
            Ok(map([
                ("exports", Value::Map(exports)),
                (
                    "names",
                    Value::Vector(module.exports.iter().cloned().map(Value::String).collect()),
                ),
                ("env", state.retain_env(&module.env)),
            ]))
        }
        _ => Err(EvalError::invalid_form(
            "unknown interpreter module operation",
        )),
    }
}

fn module_name(name: &str) -> Result<Vec<String>, EvalError> {
    let parts: Vec<String> = name.split('.').map(str::to_string).collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || part.contains(['/', '\\', ':']))
    {
        return Err(EvalError::custom(format!("invalid module name: {name}")));
    }
    Ok(parts)
}

fn zos(state: &State, args: &Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    use crate::zos::gf::{GFObject, GenericFunction, Method, MethodQualifier, Specializer};
    use crate::zos::object::{Class, ObjectFlags, SlotDefinition};
    minimum(args, 2)?;
    let env = state.env(&args[1], engine)?;
    match keyword_text(&args[0])? {
        "class" => {
            arity(args, 5)?;
            let name = string(&args[2])?.to_string();
            let mut parents = Vec::new();
            for parent in strings(&args[3])? {
                let class = match env.get(&parent) {
                    Some(Value::Object(object)) if object.header().class.name == parent => {
                        object.header().class.clone()
                    }
                    _ => Arc::new(Class {
                        name: parent.clone(),
                        superclasses: Vec::new(),
                        slots: Vec::new(),
                        cpl: vec![parent],
                    }),
                };
                parents.push(class);
            }
            let mut slots = Vec::new();
            for slot in vector_value(&args[4])? {
                let fields = match slot {
                    Value::Map(fields) if fields.len() == 3 => fields,
                    _ => {
                        return Err(EvalError::invalid_form(
                            "malformed interpreter slot metadata",
                        ));
                    }
                };
                let field = |name| {
                    fields.get(&keyword(name)).ok_or_else(|| {
                        EvalError::invalid_form("malformed interpreter slot metadata")
                    })
                };
                slots.push(SlotDefinition {
                    name: string(field("name")?)?.into(),
                    initargs: strings(field("initargs")?)?.into_iter().collect(),
                    accessor: optional_string(field("accessor")?)?,
                    initform: None,
                });
            }
            let cpl = crate::zos::class::c3_linearize(&name, &parents);
            let class = Arc::new(Class {
                name: name.clone(),
                superclasses: parents,
                slots,
                cpl,
            });
            let mut instance = crate::value::ZosInstance::new(class.clone());
            instance.header.flags = ObjectFlags::MUTABLE;
            instance
                .slots
                .insert("__class_name__".into(), Value::Symbol(name.clone()));
            env.set(name, Value::Object(Box::new(instance)));
            Ok(Value::Object(Box::new(crate::value::ZosInstance::new(
                class,
            ))))
        }
        "generic" => {
            arity(args, 4)?;
            let name = string(&args[2])?.to_string();
            let generic = Value::Object(Box::new(GFObject::new(GenericFunction::new(
                name.clone(),
                strings(&args[3])?.into_iter().collect(),
            ))));
            env.set(name, generic.clone());
            Ok(generic)
        }
        "method" => {
            arity(args, 7)?;
            let name = string(&args[2])?;
            let qualifier = match keyword_text(&args[3])? {
                "primary" => MethodQualifier::Primary,
                "before" => MethodQualifier::Before,
                "after" => MethodQualifier::After,
                "around" => MethodQualifier::Around,
                _ => return Err(EvalError::invalid_form("invalid method qualifier")),
            };
            let mut specializers = Vec::new();
            for specializer in vector_value(&args[4])? {
                specializers.push(match specializer {
                    Value::Keyword(name) if name == "t" => Specializer::T,
                    Value::String(name) => Specializer::Exact(name.clone()),
                    _ => return Err(EvalError::invalid_form("invalid method specializer")),
                });
            }
            let params = strings(&args[5])?;
            if params.len() != specializers.len() {
                return Err(EvalError::invalid_form(
                    "method specializer/parameter count mismatch",
                ));
            }
            let method = Arc::new(Method {
                specializers,
                qualifier,
                body: Arc::new(Function {
                    params,
                    rest_param: None,
                    body: stored_body(&args[6], engine)?,
                    env: env.clone(),
                    compiled: None,
                }),
            });
            let generic = env
                .get(name)
                .ok_or_else(|| EvalError::custom(format!("generic function not found: {name}")))?;
            if let Value::Object(object) = &generic {
                if let Some(generic) = crate::zos::gf::gf_shared(object.as_ref()) {
                    generic.borrow_mut().add_method(method);
                    return Ok(Value::Nil);
                }
                return Err(EvalError::type_error("GenericFunction", "non-GF object"));
            }
            Err(EvalError::type_error(
                "GenericFunction",
                generic.value_type(),
            ))
        }
        "next" => {
            arity(args, 2)?;
            let next = env.get("*next-method*").ok_or_else(|| {
                EvalError::custom("no next method available (call-next-method outside method)")
            })?;
            if !matches!(next, Value::NativeFunction(_)) {
                return Err(EvalError::custom("*next-method* is not a function"));
            }
            target_call(
                state,
                next,
                Vector::new(),
                &MetaEngine {
                    underlying: engine,
                    state,
                    root: env,
                },
            )
        }
        "package" => {
            arity(args, 5)?;
            let name = string(&args[2])?.to_string();
            let uses = strings(&args[3])?;
            let exports = strings(&args[4])?;
            let mut packages = match env.get("*packages*") {
                Some(Value::Map(packages)) => packages,
                _ => HashMap::new(),
            };
            packages.insert(keyword(&name), keyword("package"));
            env.set("*packages*".into(), Value::Map(packages));
            env.set(
                format!("*package-uses-{name}*"),
                Value::List(uses.into_iter().map(|name| keyword(&name)).collect()),
            );
            env.set(
                format!("*package-exports-{name}*"),
                Value::List(exports.into_iter().map(|name| keyword(&name)).collect()),
            );
            Ok(keyword(&name))
        }
        _ => Err(EvalError::invalid_form(
            "unknown interpreter object operation",
        )),
    }
}

struct RootEngine<'a> {
    underlying: &'a dyn EvalEngine,
    root: &'a Arc<Env>,
}
impl EvalRuntime for RootEngine<'_> {
    fn eval_expr(
        &self,
        expression: &Sexp,
        env: &Arc<Env>,
        tail: bool,
    ) -> Result<TailResult, EvalError> {
        self.underlying.eval_expr(expression, env, tail)
    }
    fn env(&self) -> &Arc<Env> {
        self.root
    }
    fn source_map(&self) -> &Arc<SourceMap> {
        self.underlying.source_map()
    }
    fn source_dir(&self) -> Option<PathBuf> {
        self.underlying.source_dir()
    }
    fn io(&self) -> &dyn crate::io::IoHost {
        self.underlying.io()
    }
    fn observation(&self) -> &crate::observer::Observation {
        self.underlying.observation()
    }
    fn spend(&self, units: u64) -> Result<(), EvalError> {
        self.underlying.spend(units)
    }
}

struct MetaEngine<'a> {
    underlying: &'a dyn EvalEngine,
    state: &'a State,
    root: Arc<Env>,
}
impl EvalRuntime for MetaEngine<'_> {
    fn eval_expr(
        &self,
        expression: &Sexp,
        env: &Arc<Env>,
        _tail: bool,
    ) -> Result<TailResult, EvalError> {
        let syntax = SyntaxNode::from_sexp(expression);
        validate_node(&syntax, self.underlying)?;
        let handle = self.state.retain_env(env);
        self.state
            .implementation_call(
                "zi-eval",
                vector![Value::Vector(vector![syntax.to_value()]), handle],
                self.underlying,
            )
            .map(TailResult::Value)
    }
    fn env(&self) -> &Arc<Env> {
        &self.root
    }
    fn source_map(&self) -> &Arc<SourceMap> {
        self.underlying.source_map()
    }
    fn source_dir(&self) -> Option<PathBuf> {
        self.underlying.source_dir()
    }
    fn io(&self) -> &dyn crate::io::IoHost {
        self.underlying.io()
    }
    fn observation(&self) -> &crate::observer::Observation {
        self.underlying.observation()
    }
    fn spend(&self, units: u64) -> Result<(), EvalError> {
        self.underlying.spend(units)
    }
}

macro_rules! delegate_modules {
    ($engine:ty) => {
        impl ModuleRegistry for $engine {
            fn register_module(&self, module: Module) {
                self.underlying.register_module(module);
            }
            fn find_module(&self, name: &[String]) -> Option<Module> {
                self.underlying.find_module(name)
            }
            fn is_module_loaded(&self, name: &[String]) -> bool {
                self.underlying.is_module_loaded(name)
            }
            fn call_loader(
                &self,
                name: &[String],
                source: &str,
                parent: &Arc<Env>,
            ) -> Option<Result<Module, EvalError>> {
                self.underlying.call_loader(name, source, parent)
            }
            fn begin_loading(&self, path: &Path) -> Result<(), EvalError> {
                self.underlying.begin_loading(path)
            }
            fn end_loading(&self, path: &Path) {
                self.underlying.end_loading(path);
            }
            fn push_module_exports(&self) {
                self.underlying.push_module_exports();
            }
            fn add_module_export(&self, name: String) {
                self.underlying.add_module_export(name);
            }
            fn take_module_exports(&self) -> Vec<String> {
                self.underlying.take_module_exports()
            }
        }
    };
}
delegate_modules!(RootEngine<'_>);
delegate_modules!(MetaEngine<'_>);

impl EvalEngine for RootEngine<'_> {
    fn intercept_native(
        &self,
        function: &NativeFn,
        args: &Vector<Value>,
    ) -> Option<Result<Value, EvalError>> {
        self.underlying.intercept_native(function, args)
    }
    fn intercept_macro(
        &self,
        mac: &Arc<Macro>,
        args: &Vector<Value>,
    ) -> Option<Result<Value, EvalError>> {
        self.underlying.intercept_macro(mac, args)
    }
}

impl EvalEngine for MetaEngine<'_> {
    fn intercept_native(
        &self,
        function: &NativeFn,
        args: &Vector<Value>,
    ) -> Option<Result<Value, EvalError>> {
        if !self.state.intrinsic_ids.contains(&function.id()) {
            return None;
        }
        Some(
            callable_info(
                self.state,
                &Value::NativeFunction(function.clone()),
                self.underlying,
            )
            .and_then(|descriptor| {
                self.state.implementation_call(
                    "zi-apply",
                    vector![descriptor, Value::Vector(args.clone())],
                    self.underlying,
                )
            }),
        )
    }
    fn intercept_macro(
        &self,
        mac: &Arc<Macro>,
        args: &Vector<Value>,
    ) -> Option<Result<Value, EvalError>> {
        Some((|| {
            let descriptor =
                callable_info(self.state, &Value::Macro(mac.clone()), self.underlying)?;
            let mut call_data = vector![Value::Symbol(mac.name.clone())];
            call_data.extend(args.iter().cloned());
            let call_node = self.state.implementation_call(
                "zi-expand-data",
                vector![
                    Value::List(call_data),
                    Value::String(mac.name.clone()),
                    Value::Nil
                ],
                self.underlying,
            )?;
            let node = SyntaxNode::from_value(&call_node)
                .ok_or_else(|| EvalError::invalid_form("malformed macro callback syntax"))?;
            let arguments = Value::Vector(
                node.children
                    .iter()
                    .skip(1)
                    .map(SyntaxNode::to_value)
                    .collect(),
            );
            let expanded = self.state.implementation_call(
                "zi-expand",
                vector![descriptor, arguments, call_node],
                self.underlying,
            )?;
            self.state
                .implementation_call("syntax->data", vector![expanded], self.underlying)
        })())
    }
}

fn finish(mut result: TailResult, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    loop {
        match result {
            TailResult::Value(value) => return Ok(value),
            TailResult::TailCall(function, args) => {
                result = crate::eval::apply(function, args, engine)?
            }
            TailResult::Recur(_) => return Err(EvalError::recur_without_loop()),
        }
    }
}

fn error_context_mut(error: &mut EvalError) -> &mut SpanContext {
    match error {
        EvalError::SymbolNotFound(_, context)
        | EvalError::InvalidForm(_, context)
        | EvalError::MacroError(_, context)
        | EvalError::Custom(_, context) => context,
        EvalError::NotAFunction { span, .. }
        | EvalError::WrongArgCount { span, .. }
        | EvalError::WrongArgCountMin { span, .. }
        | EvalError::WrongArgCountRange { span, .. }
        | EvalError::IndexOutOfBounds { span, .. }
        | EvalError::TypeError { span, .. }
        | EvalError::DivisionByZero { span }
        | EvalError::RecurNotTail { span }
        | EvalError::RecurWithoutLoop { span }
        | EvalError::Cancelled { span, .. }
        | EvalError::CapabilityDenied { span, .. }
        | EvalError::TimedOut { span, .. }
        | EvalError::LoadError(_, span) => span,
    }
}

fn keyword(name: &str) -> Value {
    Value::Keyword(name.into())
}
fn map<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Map(
        entries
            .into_iter()
            .map(|(name, value)| (keyword(name), value))
            .collect(),
    )
}
fn string(value: &Value) -> Result<&str, EvalError> {
    match value {
        Value::String(text) => Ok(text),
        _ => Err(EvalError::type_error("string", value.value_type())),
    }
}
fn optional_string(value: &Value) -> Result<Option<String>, EvalError> {
    if value == &Value::Nil {
        Ok(None)
    } else {
        Ok(Some(string(value)?.into()))
    }
}
fn keyword_text(value: &Value) -> Result<&str, EvalError> {
    match value {
        Value::Keyword(text) => Ok(text),
        _ => Err(EvalError::type_error("keyword", value.value_type())),
    }
}
fn integer(value: &Value) -> Result<usize, EvalError> {
    match value {
        Value::Integer(number) => usize::try_from(*number)
            .map_err(|_| EvalError::invalid_form("negative interpreter index")),
        _ => Err(EvalError::type_error("integer", value.value_type())),
    }
}
fn vector_value(value: &Value) -> Result<&Vector<Value>, EvalError> {
    match value {
        Value::Vector(values) => Ok(values),
        _ => Err(EvalError::type_error("vector", value.value_type())),
    }
}
fn strings(value: &Value) -> Result<Vector<String>, EvalError> {
    vector_value(value)?
        .iter()
        .map(|value| string(value).map(str::to_string))
        .collect()
}
fn arity(args: &Vector<Value>, expected: usize) -> Result<(), EvalError> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(EvalError::wrong_arg_count(expected, args.len()))
    }
}
fn minimum(args: &Vector<Value>, expected: usize) -> Result<(), EvalError> {
    if args.len() >= expected {
        Ok(())
    } else {
        Err(EvalError::wrong_arg_count_min(expected, args.len()))
    }
}
