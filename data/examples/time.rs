//! Times the walker over the home folder's measured folders: `cargo run --release --example time`.
fn main() {
    let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let roots: Vec<_> = ["Library/Caches", "Library/Developer/Xcode/DerivedData", "Documents", "Downloads", "Desktop", "Pictures", "Movies", "Music"]
        .iter().map(|p| home.join(p)).collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    for t in [1, threads] {
        let start = std::time::Instant::now();
        let folders = storage_data::walk::measure(&roots, t);
        let total: u64 = folders.iter().map(|f| f.total).sum();
        println!("{t:2} threads: {:.2} s, {} bytes", start.elapsed().as_secs_f64(), total);
    }
}
