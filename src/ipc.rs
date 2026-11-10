use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::thread;

pub const IPC_PORT: u16 = 49271;

pub fn send_command(command: &str) -> Result<()> {
    let mut stream = TcpStream::connect(("127.0.0.1", IPC_PORT))
        .context("No se pudo conectar con la instancia principal de hypr-dictate. ¿Está iniciada?")?;
    writeln!(stream, "{command}")?;
    stream.flush()?;
    Ok(())
}

pub fn start_listener(tx: Sender<String>) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", IPC_PORT))
        .context("No se pudo iniciar el listener IPC (puerto en uso)")?;

    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            thread::spawn(move || {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    let cmd = line.trim().to_lowercase();
                    if !cmd.is_empty() {
                        let _ = tx.send(cmd);
                    }
                }
            });
        }
    });

    Ok(())
}
