//! # Cocoon 🦋
//!
//! [`Cocoon`] is a protected container to wrap sensitive data with a strong encryption
//! and format validation. The format of the [`Cocoon`] is developed to be used for the following
//! practical cases:
//!
//! 1. As a _file format_ to organize a simple secure storage:
//!    1. Key store.
//!    2. Password store.
//!    3. Sensitive data store.
//! 2. _Encrypted data transfer_:
//!    * As a secure in-memory container.

#![forbid(unsafe_code)]
#![warn(missing_docs, unused_qualifications)]
#![cfg_attr(not(feature = "std"), no_std)]

mod error;
mod format;
mod header;
mod kdf;

#[cfg(feature = "alloc")]
extern crate alloc;

use aes_gcm::Aes256Gcm;
use chacha20poly1305::{
    aead::{generic_array::GenericArray, Aead, NewAead},
    ChaCha20Poly1305,
};
#[cfg(feature = "std")]
use rand::rngs::ThreadRng;
use rand::{
    rngs::StdRng,
    {CryptoRng, RngCore, SeedableRng},
};

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
#[cfg(feature = "std")]
use std::io::{Read, Write};
use std::marker::PhantomData;

use format::{FormatPrefix, FormatVersion1};
use header::{CocoonConfig, CocoonHeader};

pub use error::Error;
pub use header::{CocoonCipher, CocoonKdf};

type EnableWrapMethods = u8;
type EnableParseMethods = u16;

/// Hides data into encrypted container.
///
/// # Basic Usage
/// ```
/// use cocoon::Cocoon;
///
/// let cocoon = Cocoon::new(b"password");
///
/// let wrapped = cocoon.wrap(b"my secret data").unwrap();
/// let unwrapped = cocoon.unwrap(&wrapped).unwrap();
///
/// assert_ne!(&wrapped, b"my secret_data");
/// assert_eq!(unwrapped, b"my secret_data");
/// ```
///
/// # Default Configuration
/// | Option           | Value                          |
/// |------------------|--------------------------------|
/// | Cipher           | Chacha20Poly1305               |
/// | Key derivation   | PBKDF2 with 100 000 iterations |
/// | Random generator | [`ThreadRng`]                  |
///
/// * Cipher can be customized using [`with_cipher`](Cocoon::with_cipher) method.
/// * Key derivation (KDF): only PBKDF2 is supported.
/// * Random generator:
///   - [`ThreadRng`] in `std` build.
///   - [`StdRng`] in "no std" build: use [`from_rng`](Cocoon::from_rng) and
///     [`from_entropy`](Cocoon::from_entropy) functions.
pub struct Cocoon<'a, R: CryptoRng + RngCore + Clone, W> {
    password: &'a [u8],
    rng: R,
    config: CocoonConfig,
    _methods_marker: PhantomData<W>,
}

#[cfg(feature = "std")]
impl<'a> Cocoon<'a, ThreadRng, EnableWrapMethods> {
    /// Creates a new `Cocoon` with [`ThreadRng`] random generator
    /// and a [Default Configuration](#default-configuration).
    ///
    /// # Arguments
    ///
    /// `password` - a shared reference to a password.
    ///
    pub fn new(password: &'a [u8]) -> Self {
        Cocoon {
            password,
            rng: ThreadRng::default(),
            config: CocoonConfig::default(),
            _methods_marker: PhantomData,
        }
    }
}

impl<'a> Cocoon<'a, StdRng, EnableWrapMethods> {
    /// Creates a new `Cocoon` using a standard random generator with seed.
    ///
    /// The method can be used when ThreadRnd is not accessible in "no std" build.
    /// **WARNING**: Use this method carefully, don't feed it with a static seed unless testing!
    pub fn from_seed(password: &'a [u8], seed: [u8; 32]) -> Self {
        Cocoon {
            password,
            rng: StdRng::from_seed(seed),
            config: CocoonConfig::default(),
            _methods_marker: PhantomData,
        }
    }

    /// Creates a new `Cocoon` using a third party random generator.
    ///
    /// The method can be used when ThreadRnd is not accessible in "no std" build.
    pub fn from_rng<R: RngCore>(password: &'a [u8], rng: R) -> Result<Self, rand::Error> {
        Ok(Cocoon {
            password,
            rng: StdRng::from_rng(rng)?,
            config: CocoonConfig::default(),
            _methods_marker: PhantomData,
        })
    }

    #[cfg(feature = "getrandom")]
    /// Creates a new `Cocoon` using OS random generator from [`SeedableRng::from_entropy`].
    ///
    /// The method can be used to create a `Cocoon` when [`ThreadRng`] is not accessible
    /// in "no std" build.
    pub fn from_entropy(password: &'a [u8]) -> Self {
        Cocoon {
            password,
            rng: StdRng::from_entropy(),
            config: CocoonConfig::default(),
            _methods_marker: PhantomData,
        }
    }
}

impl<'a> Cocoon<'a, NoRng, EnableParseMethods> {
    /// Creates a [`Cocoon`] instance with no accessible creation methods like [`Cocoon::wrap()`],
    /// [`Cocoon::dump()`] and [`Cocoon::encrypt()`].
    ///
    /// This is needed if you don't want to encrypt a container, and only to decrypt/parse one.
    /// All encryption methods need a cryptographic random generator to generate salt and nonces,
    /// and at the opposite side parsing doesn't need one, therefore `parse_only` could be suitable
    /// in a limited embedded environment, or if need a simple approach just to unwrap a cocoon.
    pub fn parse_only(password: &'a [u8]) -> Self {
        Cocoon {
            password,
            rng: NoRng,
            config: CocoonConfig::default(),
            _methods_marker: PhantomData,
        }
    }
}

/// Wrapping/encryption methods are accessible only when random generator is accessible.
impl<'a, R: CryptoRng + RngCore + Clone> Cocoon<'a, R, EnableWrapMethods> {
    /// Sets encryption algorithm to wrap data on.
    ///
    /// # Examples
    ///
    /// Basic usage:
    /// ```
    /// //let cocoon = Cocoon::new(b"password").with_cipher(CocoonCipher::Aes256Gcm);
    /// //cocoon.wrap(b"my secret data");
    ///
    /// //let cocoon = Cocoon::new(b"password").with_cipher(CocoonCipher::Chacha20Poly1305);
    /// //cocoon.wrap(b"my secret data");
    /// ```
    pub fn with_cipher(mut self, cipher: CocoonCipher) -> Self {
        self.config = self.config.with_cipher(cipher);
        self
    }

    /// Reduces a number of iterations for key derivation function (KDF).
    ///
    /// This modifier could be used for testing in debug mode, and should not be used
    /// in a production and release builds.
    pub fn with_weak_kdf(mut self) -> Self {
        self.config = self.config.with_weak_kdf();
        self
    }

    /// Wraps data into an encrypted container.
    #[cfg(feature = "alloc")]
    pub fn wrap(&self, data: &[u8]) -> Result<Vec<u8>, Error> {
        const PREFIX_SIZE: usize = FormatVersion1::size();

        // Allocation is needed because there is no way to prefix encrypted
        // data with a header without an allocation. It means that we need
        // to copy data at least once. It's necessary to avoid any further copying.
        let mut container = Vec::with_capacity(PREFIX_SIZE + data.len());
        container.extend_from_slice(&[0; PREFIX_SIZE]);
        container.extend_from_slice(data);

        let body = &mut container[PREFIX_SIZE..];

        // Encrypt in place and get a prefix part.
        let detached_prefix = self.encrypt(body)?;

        container[..PREFIX_SIZE].copy_from_slice(&detached_prefix);

        Ok(container)
    }

    /// Encrypts data in place and dumps the container into the writer ([`std::fs::File`],
    /// [`std::io::Cursor`], etc).
    #[cfg(feature = "std")]
    pub fn dump(&mut self, mut data: Vec<u8>, writer: &mut impl Write) -> Result<(), Error> {
        let detached_prefix = self.encrypt(&mut data)?;

        writer.write_all(&detached_prefix)?;
        writer.write_all(&data)?;

        Ok(())
    }

    /// Encrypts data in place and returns a formatted prefix of the container.
    ///
    /// The prefix is needed to decrypt data with [`Cocoon::decrypt`].
    /// This method doesn't use memory allocation and it is suitable with no [`std`]
    /// and no [`alloc`] build.
    pub fn encrypt(&self, data: &mut [u8]) -> Result<[u8; FormatVersion1::size()], Error> {
        let mut rng = self.rng.clone();

        let mut salt = [0u8; 16];
        let mut nonce = [0u8; 12];

        rng.fill_bytes(&mut salt);
        rng.fill_bytes(&mut nonce);

        let header =
            CocoonHeader::new(self.config.clone(), salt, nonce, data.len() as u64).serialize();

        let master_key = match self.config.kdf() {
            CocoonKdf::Pbkdf2 => {
                kdf::pbkdf2::derive(&salt, self.password, self.config.kdf_iterations())
            }
        };

        let nonce = GenericArray::from_slice(&nonce);
        let master_key = GenericArray::clone_from_slice(master_key.as_ref());

        let tag: [u8; 16] = match self.config.cipher() {
            CocoonCipher::Chacha20Poly1305 => {
                let cipher = ChaCha20Poly1305::new(master_key);
                cipher.encrypt_in_place_detached(nonce, &header, data)
            }
            CocoonCipher::Aes256Gcm => {
                let cipher = Aes256Gcm::new(master_key);
                cipher.encrypt_in_place_detached(nonce, &header, data)
            }
        }
        .map_err(|_| Error::Cryptography)?
        .into();

        Ok(FormatVersion1::serialize(&header, &tag))
    }
}

/// Parsing methods are always accessible. They don't need random generator in general.
impl<'a, R: CryptoRng + RngCore + Clone, W> Cocoon<'a, R, W> {
    /// Creates a new `Cocoon` using any third party random generator.
    pub fn from_crypto_rng(password: &'a [u8], rng: R) -> Self {
        Cocoon {
            password,
            rng,
            config: CocoonConfig::default(),
            _methods_marker: PhantomData,
        }
    }

    /// Unwraps data from the wrapped format (see [`Cocoon::wrap`]).
    #[cfg(feature = "alloc")]
    pub fn unwrap(&self, container: &[u8]) -> Result<Vec<u8>, Error> {
        let prefix = FormatPrefix::deserialize(&container)?;
        let header = CocoonHeader::deserialize(prefix.header())?;
        let mut body = Vec::with_capacity(header.data_length() as usize);

        self.decrypt_parsed(&mut body, &prefix, &header)?;

        Ok(body)
    }

    /// Parses container from the reader (file, cursor, etc.), validates format,
    /// allocates memory and places decrypted data there.
    #[cfg(feature = "std")]
    pub fn parse(&self, reader: &mut impl Read) -> Result<Vec<u8>, Error> {
        let prefix = FormatPrefix::deserialize_from(reader)?;
        let header = CocoonHeader::deserialize(prefix.header())?;
        let mut body = Vec::with_capacity(header.data_length() as usize);

        reader.read_exact(&mut body)?;

        self.decrypt_parsed(&mut body, &prefix, &header)?;

        Ok(body)
    }

    /// Decrypts data in place using the parts returned by `encrypt` method.
    ///
    /// The method doesn't use memory allocation and is suitable for "no std" and "no alloc" build.
    pub fn decrypt(&self, data: &mut [u8], prefix: &[u8]) -> Result<(), Error> {
        let prefix = FormatPrefix::deserialize(prefix)?;
        let header = CocoonHeader::deserialize(prefix.header())?;

        self.decrypt_parsed(data, &prefix, &header)
    }

    fn decrypt_parsed(
        &self,
        data: &mut [u8],
        format: &FormatPrefix,
        header: &CocoonHeader,
    ) -> Result<(), Error> {
        let mut salt = [0u8; 16];
        let mut nonce = [0u8; 12];

        salt.copy_from_slice(header.salt());
        nonce.copy_from_slice(header.nonce());

        let master_key = match header.config().kdf() {
            CocoonKdf::Pbkdf2 => {
                kdf::pbkdf2::derive(&salt, self.password, header.config().kdf_iterations())
            }
        };

        let nonce = GenericArray::from_slice(&nonce);
        let master_key = GenericArray::clone_from_slice(master_key.as_ref());
        let tag = GenericArray::from_slice(&format.tag());

        match header.config().cipher() {
            CocoonCipher::Chacha20Poly1305 => {
                let cipher = ChaCha20Poly1305::new(master_key);
                cipher.decrypt_in_place_detached(nonce, &format.header(), data, tag)
            }
            CocoonCipher::Aes256Gcm => {
                let cipher = Aes256Gcm::new(master_key);
                cipher.decrypt_in_place_detached(nonce, &format.header(), data, tag)
            }
        }
        .map_err(|_| Error::Cryptography)?;

        Ok(())
    }
}

#[derive(Clone)]
struct NoRng;

impl CryptoRng for NoRng {}
impl RngCore for NoRng {
    fn next_u32(&mut self) -> u32 {
        unreachable!();
    }
    fn next_u64(&mut self) -> u64 {
        unreachable!();
    }
    fn fill_bytes(&mut self, _dest: &mut [u8]) {
        unreachable!();
    }
    fn try_fill_bytes(&mut self, _dest: &mut [u8]) -> Result<(), rand::Error> {
        unreachable!();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn cocoon_encrypt() {
        let cocoon = Cocoon::from_seed(b"password", [0; 32]).with_weak_kdf();
        let mut data = "my secret data".to_owned().into_bytes();

        let detached_prefix = cocoon.encrypt(&mut data).unwrap();

        assert_eq!(
            &[
                127, 192, 10, 1, 1, 1, 2, 0, 118, 184, 224, 173, 160, 241, 61, 144, 64, 93, 106,
                229, 83, 134, 189, 40, 189, 210, 25, 184, 160, 141, 237, 26, 168, 54, 239, 204, 0,
                0, 0, 0, 0, 0, 0, 14, 245, 24, 39, 167, 173, 32, 174, 247, 250, 85, 17, 250, 119,
                96, 187, 207
            ][..],
            &detached_prefix[..]
        );

        assert_eq!(
            &[168, 128, 133, 25, 121, 30, 206, 73, 191, 115, 252, 164, 158, 240],
            &data[..]
        );
    }

    #[test]
    fn cocoon_decrypt() {
        let detached_prefix = [
            127, 192, 10, 1, 1, 1, 1, 0, 118, 184, 224, 173, 160, 241, 61, 144, 64, 93, 106, 229,
            83, 134, 189, 40, 189, 210, 25, 184, 160, 141, 237, 26, 168, 54, 239, 204, 0, 0, 0, 0,
            0, 0, 0, 14, 53, 9, 86, 247, 53, 186, 123, 217, 156, 132, 173, 200, 208, 134, 179, 12,
        ];
        let mut data = [
            244, 85, 222, 144, 119, 169, 144, 11, 178, 216, 4, 57, 17, 47,
        ];
        let cocoon = Cocoon::parse_only(b"password");

        cocoon
            .decrypt(&mut data, &detached_prefix)
            .expect("Decrypted data");

        assert_eq!(b"my secret data", &data);
    }
}