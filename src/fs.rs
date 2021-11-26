/// Opens a file and returns a useful error if it fails
/// If possible use this instead of File::open for usability purposes
pub fn open_file<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<std::fs::File> {
    match std::fs::File::open(&path) {
        Err(why) => Err(std::io::Error::new(why.kind(), format!("couldn't open {}", path.display()))),
        Ok(file) => Ok(file),
    }
}
