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
pub const LEAF_SIGNATURE_OF_WISP: &str = "2c6e6ecc0fa253f983e97216e78c56a9a56f544c857bd4bf30f083c78fe09675a1d987dcd9782bf33e04540e0fab9aa8190964ae463712f0d12c207885c148fca368cf6b607613937406c74c5f974b3f64db22b93b9bac1988603325f65c33cad28e3fdac582b8321b492e2b9fa4d5d36542863f577ed5e207e8305826a31adfbf4b2df66460af3a1749361418cfab553014dc4848884095d8dbe821b160d7501b58b38efdc2120ca768b73ab178db10930e9753a03a91060e9f5f332723bc69bfe055e0f570ed19bab7c5ea43e33581cef78134e15671e5b069258fab15765a1d6ae22d465ae71f585c2f5f75eb100f21e2a759d221cf1d640e92b7218fd6e3";
/// The leaf certificate's SHA-1 thumbprint and serial number.
pub const LEAF_THUMBPRINT: &str = "107E4BF3DCE2873C21515EC5B414A3AB0B7C2BE6";
pub const LEAF_SERIAL: &str = "2F0532B8E149C702CDBADE10084FF65026B6B881";
