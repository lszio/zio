pub mod core;

use std::io::{self, Write};
use std::sync::Arc;
use crate::core::env::Env;
use crate::core::reader;
use crate::core::eval;
use crate::core::builtins;

fn main() {
    let env = Arc::new(Env::new(None));
    builtins::setup_env(env.clone());

    println!("Zio REPL");
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
                    Ok(val) => {
                        match eval::eval(val, env.clone()) {
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
