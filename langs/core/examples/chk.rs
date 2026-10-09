fn main() {
    let path = std::env::args().nth(1).expect("path");
    let src = std::fs::read_to_string(&path).expect("read");
    let map = zio_core::span::SourceMap::new();
    match zio_core::syntax::inspect_source(&map, "x.zio", &src) {
        Ok(n) => println!("parsed {} forms", n.len()),
        Err(e) => {
            println!("{e}");
            if let Some(s) = e.span() {
                println!(
                    "line {} col {} bytes {}..{}",
                    s.line, s.col, s.start.0, s.end.0
                );
            }
        }
    }
}
