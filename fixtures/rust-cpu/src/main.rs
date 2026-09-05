fn main() {
    let mut value = 0u64;
    for index in 0..20_000_000u64 {
        value = value
            .wrapping_mul(1664525)
            .wrapping_add(index ^ 1013904223);
    }
    println!("cpu={value}");
}
