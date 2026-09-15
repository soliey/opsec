fn main() {
    match capture::capture_one_frame() {
        Ok(frame) => println!(
            "captured {}x{} frame, {} bytes",
            frame.width,
            frame.height,
            frame.data.len()
        ),
        Err(e) => println!("capture failed: {e}"),
    }
}
