//! Une seule instance de ytmr à la fois.
//!
//! En clair : la première instance écoute sur un port local (127.0.0.1, rien
//! n'est ouvert sur le réseau). Une deuxième, lancée alors que la première
//! tourne (même cachée dans la zone de notification), s'y annonce : la
//! première ramène sa fenêtre et la deuxième s'arrête aussitôt. Avant, deux
//! instances lançaient deux mpv et s'écrasaient leur session.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

/// Port de signal entre instances (juste après celui de l'API).
const PORT: u16 = 26541;
const HELLO: &str = "ytmr-show";
const REPLY: &str = "ytmr-ok";

/// `true` : cette instance est la seule (elle continue). `false` : une autre
/// tourne déjà, sa fenêtre a été ramenée ; celle-ci doit s'arrêter.
pub fn claim() -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, PORT));
    match TcpListener::bind(addr) {
        Ok(listener) => {
            let spawned = std::thread::Builder::new()
                .name("ytmr-instance".into())
                .spawn(move || serve(listener));
            if let Err(e) = spawned {
                tracing::warn!(error = %e, "instance unique");
            }
            true
        }
        // Port pris : une instance de ytmr (ou un autre programme).
        Err(_) => !announce(addr),
    }
}

/// Une autre instance se lance : on ramène la fenêtre.
fn serve(listener: TcpListener) {
    for stream in listener.incoming().flatten() {
        let mut line = String::new();
        let mut reader = BufReader::new(&stream);
        if reader.read_line(&mut line).is_ok() && line.trim() == HELLO {
            let _ = (&stream).write_all(format!("{REPLY}\n").as_bytes());
            tracing::info!("deuxième lancement : fenêtre ramenée");
            #[cfg(windows)]
            ytmr_os::tray::show();
        }
    }
}

/// `true` si c'est bien ytmr qui a répondu (il a ramené sa fenêtre).
fn announce(addr: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    #[cfg(windows)]
    ytmr_os::tray::allow_foreground();
    if stream.write_all(format!("{HELLO}\n").as_bytes()).is_err() {
        return false;
    }
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).is_ok() && line.trim() == REPLY
}
