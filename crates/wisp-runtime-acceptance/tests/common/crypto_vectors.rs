//! Known answers for the cryptography family: standard vectors and the
//! values computed with OpenSSL over the fixed material the guest embeds.

/// SHA-256 of "abc" (FIPS 180-2 example).
pub const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
/// HMAC-SHA256, RFC 4231 test case 2 (key "Jefe").
pub const HMAC_JEFE: &str = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
/// HKDF-SHA256, RFC 5869 test case 1, 42 bytes.
pub const HKDF_RFC5869_1: &str =
    "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865";
/// PBKDF2-HMAC-SHA256("password", "salt", 1 iteration, 32 bytes).
pub const PBKDF2_ONE: &str = "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b";
/// AES-128-GCM, NIST GCM test case 2: zero key, zero IV, one zero block.
pub const AES_GCM_CIPHERTEXT: &str = "0388dace60b6a392f328c2b971b2fe78";
pub const AES_GCM_TAG: &str = "ab6e47d42cec13bdf53a67b21257bddf";
/// RSA PKCS#1 v1.5 SHA-256 signature of "wisp" by the leaf key.
pub const LEAF_SIGNATURE_OF_WISP: &str = "8c44c4110fd4623f52d52fbc459a0ffc985b9107de3f271068404ae1eeb1c06e3d8a6d498907a5fe95c0c3309acfb9152097ac3e02a0f0992718a7ec4406fc2df1190fc3fd52548d598304acc3e9a706d91053e2f9debe18e7dd708185d03b46dcba0c642ee60a1b4f503e536fecc1c276434737735a5e9fdc622a3fcb1649079fb4b53e2e45c551a13385336972a9bb0f4719cc104354364421209265f1a176690b31c189728a49937d2537365ca004bc00351a52b5e215095ba022bac95ac4cb18665cacea3ca2273e84f2536101418b5f44ca788ccd4a4a4b1b540b8f1f336c410822153b4b3b881f8ce8b642eeeec0d5420c287414d58698ef1bc3d72632";
/// The leaf certificate's SHA-1 thumbprint and serial number.
pub const LEAF_THUMBPRINT: &str = "46288802F57927AAE9799B9A8C859934D7A754FB";
pub const LEAF_SERIAL: &str = "41E0BF8C03D3A212A03682592C17F7F3A60A2B6F";
