fn main() {
    let mut chunks = Vec::new();
    loop {
        chunks.push(vec![0u8; 1024 * 1024]);
        std::hint::black_box(&chunks);
    }
}
