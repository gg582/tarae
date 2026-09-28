mod parse;

/// Reads `host:port` pairs and prints the ones that parse.
fn main() {
    let lines = ["127.0.0.1:8080", "nope", "[::1]:443"];
    for line in lines {
        match parse::addr(line) {
            Some((host, port)) => println!("{host} → {port}"),
            None => eprintln!("skipped {line:?}"),
        }
    }
}
