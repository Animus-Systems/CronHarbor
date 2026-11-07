mod cli;
fn main() {
    match cli::run() {
        Ok(value) => {
            if !value.is_null() {
                println!("{value}");
            }
        }
        Err(error) => {
            eprintln!("{}", serde_json::json!({"error": error}));
            std::process::exit(1);
        }
    }
}
