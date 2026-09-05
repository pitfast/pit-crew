fn main() {
    #[cfg(target_env = "p2")]
    println!("Hello from PitCrew P2!");
    #[cfg(not(target_env = "p2"))]
    println!("Hello from PitCrew!");

    for arg in std::env::args().skip(1) {
        println!("arg={arg}");
    }
    if let Ok(mode) = std::env::var("MODE") {
        println!("MODE={mode}");
    }
}
