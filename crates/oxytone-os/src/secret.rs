//! Chiffrement de la session YouTube gardée sur le disque.
//!
//! En clair : sous Windows, la session (cookies de connexion) est chiffrée
//! avec la protection de données de Windows (DPAPI) : seul ton compte
//! Windows, sur ce PC, peut la relire. Ailleurs, le fichier est seulement
//! réservé à l'utilisateur (droits 600).

/// Chiffre `data` pour l'utilisateur courant.
pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    imp::protect(data)
}

/// Relit ce que [`protect`] a chiffré (même utilisateur, même machine).
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    imp::unprotect(data)
}

#[cfg(windows)]
#[allow(unsafe_code)] // API Win32 (DPAPI), sans équivalent sûr.
mod imp {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::PCWSTR;

    fn run(data: &[u8], encrypt: bool) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(data.len()).map_err(|e| e.to_string())?,
            pbData: data.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        let result = unsafe {
            if encrypt {
                CryptProtectData(
                    &input,
                    PCWSTR::null(),
                    None,
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    None,
                    None,
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            }
        };
        result.map_err(|e| e.to_string())?;
        // Copie, puis libération du tampon alloué par Windows.
        let bytes =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
        unsafe {
            let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        }
        Ok(bytes)
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        run(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        run(data, false)
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.to_vec())
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        Ok(data.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let secret = b"SAPISID=abc; SID=def";
        let sealed = protect(secret).unwrap();
        if cfg!(windows) {
            assert_ne!(sealed.as_slice(), secret.as_slice(), "chiffré");
        }
        assert_eq!(unprotect(&sealed).unwrap(), secret);
    }
}
