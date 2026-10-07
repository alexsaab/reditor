use std::io::{self, Write};

fn main() -> io::Result<()> {
    let name = if let Some(name) = std::env::args().nth(1) {
        name
    } else {
        print!("Как вас зовут? Чукапабро?! ");
        io::stdout().flush()?;
        let mut name = String::new();
        io::stdin().read_line(&mut name)?;
        name
    };
    println!("Привет, {}! Rust-приложение работает 🦀", name.trim());
    Ok(())
}
