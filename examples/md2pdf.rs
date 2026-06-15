fn main() {
    let path = std::env::args().nth(1).expect("usage: md2pdf <file.md> [out.pdf]");
    let out = std::env::args().nth(2).unwrap_or_else(|| "/tmp/out.pdf".to_string());
    let bytes = tui_pdf::markdown::render_to_pdf_bytes(std::path::Path::new(&path))
        .expect("render");
    std::fs::write(&out, &bytes).expect("write");
    println!("wrote {} bytes to {}", bytes.len(), out);
}
