//! Une seule instance d'oxytone à la fois.
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
const HELLO: &str = "oxytone-show";
const REPLY: &str = "oxytone-ok";

/// Résultat de [`claim`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instance {
    /// Seule instance : on continue.
    Alone,
    /// Un oxytone tourne déjà, sa fenêtre a été ramenée : on s'arrête.
    Exit,
    /// Le port est tenu par un programme qui ne répond pas comme oxytone :
    /// en pratique ytmr (l'ancien nom), qui écoute sur le même port. On
    /// continue, sans instance unique.
    Foreign,
}

pub fn claim(wait: bool) -> Instance {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, PORT));
    // Redémarrage : l'ancienne instance libère le port en quelques instants.
    let mut bound = TcpListener::bind(addr);
    if wait {
        for _ in 0..50 {
            if bound.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            bound = TcpListener::bind(addr);
        }
    }
    match bound {
        Ok(listener) => {
            let spawned = std::thread::Builder::new()
                .name("oxytone-instance".into())
                .spawn(move || serve(listener));
            if let Err(e) = spawned {
                tracing::warn!(error = %e, "instance unique");
            }
            Instance::Alone
        }
        // Port pris : une instance d'oxytone (ou un autre programme).
        Err(_) if announce(addr) => Instance::Exit,
        Err(_) => Instance::Foreign,
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
            oxytone_os::tray::show();
        }
    }
}

/// `true` si c'est bien oxytone qui a répondu (il a ramené sa fenêtre).
fn announce(addr: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    #[cfg(windows)]
    oxytone_os::tray::allow_foreground();
    if stream.write_all(format!("{HELLO}\n").as_bytes()).is_err() {
        return false;
    }
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).is_ok() && line.trim() == REPLY
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un programme tient le port d'instance sans répondre comme oxytone :
    /// c'est ce que fait ytmr (l'ancien nom), qui lit le message, ne le
    /// reconnaît pas et ne répond rien. → `Foreign` (la migration attendra).
    /// Port réel (26541) : ni ytmr ni oxytone ne doivent être ouverts.
    #[test]
    #[ignore = "utilise le port 26541 (ni ytmr ni oxytone ouverts)"]
    fn a_program_that_does_not_answer_like_oxytone_is_foreign() {
        let fake_ytmr = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, PORT))).unwrap();
        let ytmr = std::thread::spawn(move || {
            let (stream, _) = fake_ytmr.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            line
        });
        assert_eq!(claim(false), Instance::Foreign);
        assert_eq!(ytmr.join().unwrap().trim(), HELLO);
    }
}
