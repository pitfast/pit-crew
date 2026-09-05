fn main() {
    let mut value = 0u64;
    loop {
        value = value.wrapping_add(1);
        std::hint::black_box(value);
    }
}
