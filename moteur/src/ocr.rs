//! OCR intégré à Windows (Windows.Media.Ocr) : gratuit, local, langues du profil. Ailleurs, pas encore :
//! l'indexation s'en passe (le texte des images et des scans n'est alors pas lu).

#[cfg(windows)]
pub use windows_ocr::Ocr;
#[cfg(not(windows))]
pub use sans_ocr::Ocr;

#[cfg(windows)]
mod windows_ocr {
    use crate::photo::Pixels;
    use anyhow::{Context, Result};
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;

    pub struct Ocr {
        moteur: OcrEngine,
        pub cote_max: u32,
    }

    impl Ocr {
        pub fn nouveau() -> Result<Self> {
            let moteur = OcrEngine::TryCreateFromUserProfileLanguages().context("OCR Windows indisponible")?;
            Ok(Ocr { moteur, cote_max: OcrEngine::MaxImageDimension()? })
        }

        pub fn langue(&self) -> String {
            self.moteur.RecognizerLanguage().and_then(|l| l.DisplayName()).map(|n| n.to_string()).unwrap_or_default()
        }

        /// Lit le texte d'une image BGRA.
        pub fn lire(&self, p: &Pixels) -> Result<String> {
            let ecrivain = DataWriter::new()?;
            ecrivain.WriteBytes(&p.octets)?;
            let image = SoftwareBitmap::CreateCopyFromBuffer(
                &ecrivain.DetachBuffer()?,
                BitmapPixelFormat::Bgra8,
                p.largeur as i32,
                p.hauteur as i32,
            )?;
            let resultat = self.moteur.RecognizeAsync(&image)?.join()?;
            let mut texte = String::new();
            for ligne in resultat.Lines()? {
                texte.push_str(&ligne.Text()?.to_string());
                texte.push('\n');
            }
            Ok(texte)
        }
    }
}

#[cfg(not(windows))]
mod sans_ocr {
    use crate::photo::Pixels;
    use anyhow::{Result, bail};

    pub struct Ocr {
        pub cote_max: u32,
    }

    impl Ocr {
        pub fn nouveau() -> Result<Self> {
            bail!("pas encore d'OCR sur ce système")
        }

        pub fn langue(&self) -> String {
            String::new()
        }

        pub fn lire(&self, _p: &Pixels) -> Result<String> {
            bail!("pas encore d'OCR sur ce système")
        }
    }
}

/// Assez de vrais mots pour valoir la peine d'être indexé (une photo de paysage donne souvent du bruit).
pub fn texte_utile(texte: &str) -> bool {
    let mots = texte.split_whitespace().filter(|m| m.chars().filter(|c| c.is_alphabetic()).count() >= 3).count();
    mots >= 4
}
