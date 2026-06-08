use std::io::{self, Write};
use std::sync::Arc;

use zio_core::env::Env;
use zio_core::eval;
use zio_core::builtins;
use zio_reader::reader;

fn main() {
    let env = Arc::new(Env::new(None));
    builtins::setup_env(&env);

    println!("Zio REPL (v0.2 - architecture refactor)");
    println!("Press Ctrl+D or type (exit) to quit");

    loop {
        print!("zio> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(0) => {
                println!();
                break;
            }
            Ok(_) => {
                let input = input.trim();
                if input.is_empty() {
                    continue;
                }

                if input == "(exit)" {
                    break;
                }

                match reader::read(input) {
                    Ok(sexp) => {
                        match eval::eval(&sexp, &env) {
                            Ok(res) => println!("{}", res),
                            Err(e) => println!("Error: {}", e),
                        }
                    }
                    Err(e) => println!("Parse Error: {}", e),
                }
            }
            Err(e) => {
                println!("Error reading input: {}", e);
                break;
            }
        }
    }
}