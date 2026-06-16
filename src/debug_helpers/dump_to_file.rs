use std::fs::File;
use std::io::{BufWriter, Write};
pub fn dump_to_file<T: std::fmt::Debug>(data: &[T], filename: &str){
    let file = File::create(filename).unwrap();
    let mut writer = BufWriter::new(file);
    for item in data{
        writeln!(writer, "{:?}", item).unwrap();
    }
    writer.flush().unwrap();
}