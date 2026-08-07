//! A throwaway 2048-bit RSA private key, for tests that need `AppAuth` to
//! mint a well-formed JWT. This key has never been registered with GitHub
//! (nor with anything else) and belongs to no App -- it exists only so
//! `jsonwebtoken::EncodingKey::from_rsa_pem` has a syntactically valid PEM to
//! sign against; the mock server in `mock_github.rs` never verifies the
//! JWT's signature. Generated once with `openssl genrsa -traditional 2048`.
//! Test-only: unreachable from any production path (AGENTS.md).

pub const TEST_APP_PRIVATE_KEY_PEM: &str = "-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEAtOjy/gO/A5Sc9pxxdUxY3gpjCwwcPmAeCSUQRXxUpVQoo17I
Ph+2S018GH+wn2kLbUJJk9tYCtKAwApRso/rb/8IKpWm3Ft+ecBnF+H1pU+AFRWk
DrTD7AUCcXPqR3stLCvE95jq7NFHRnMfv0OXaB/obUBCgT2FySPrX8Rd2fKLazrK
ZFTXRR10pl8mVf3cOqB529fDxyHthP1J1s7iu4j+HDkO89xxm6Fwfbp+71Yz7XH5
yApsFMrXbrMv98B9z00d6muh+EcszmYu/JBrf1kcdc+Rt12svqs1uGTqsgWic7ef
rka3dzecCP180B/uIEQJyjE8vlSyw6PNI+nmLwIDAQABAoIBAEFt0NhS1YY/fQdq
LFSukKN5oTmRHzPmAmbvSzO+VETZK7tuX8CsKnuQohWgNOpqjPHum/rIRU7gtCUA
dmy8xXtjgvoX1tnyk0sIbaDDHds0Zg/6HDQfZ46Yfzo2IKDKqVtE1z9vRGPzCrKt
l2lO0lcb1y2QJJ1meVj2Tz37ILBefnnhDvYZvWOiIzRC7ZvsNApdQ2VVDN9Cj9b2
Cj0SXZ78gU9BeVW3KpiukHHLaja07rUwapo7EyCYntfyNwPGas5Rxmh4FDwGANRl
y3Onv/PByswblYIGyVw2Vk+PeYf4hdusYuNZ0rReMxttlbYIDW4v2Ok1BDPCmryC
CBPtQnUCgYEA7E6C5k5oWOlvzR1PQ04nZSAr/4pysqupZS0bUtKgexfhHIC4MmO6
QOpKTAXt36mG/ZpMnBLy60qNfABd6wDwygmWFYHH9S6VocDnV3oDWMfqD+hKWbKL
HMV8JXwPpXwjn4e9U22s8+Efh7HOvxx0dXSduwJqniUFfb0dMigcmCMCgYEAw/yT
VTQWaoCvph3AdKJpbb/UZ8yj82hoxKMZV7I8g4A6hyZn+ePOLh+0Peef0kbncxZd
Ug6yV144gMHIa3v/wMSqnBkkXd9fgPiymuc56GDZOogl/TgubBrR7LVYQXaZYo2Z
kqiXaK0P/XgRdC5mJJ+aoZv0Exn08m2XsOHVdIUCgYAVZlrGXo1ml+VPDvtxne9F
Yi952eDfO1qA1h/mVTrBSv1Q5ntH3O4uGMmXruXG3oRiDQopDDJBiqPbefEHajNk
KJAV7IXeN1THrD+HFX6eGKSiwieRjfC5L005281S8DYNqW5E0ubZwyZm1HxjpEEL
rf7mw6ZCIhooM+sj8qv8PwKBgQCGJAHTd2tASgPu9r4bFm6Cp6GByhcNKpFKxTc7
RssUVle42RiheMJN33VGSZqiGdWgd9Y3q8d09RBHUFsU9jH+hp0fajXx6kk7xPy5
+TkxS9hir30Q67saUuEL2rMlWz9wrOpH7wxyoMEpA10u3/MZbgQwSMWtrT5yD4Cb
mHa44QKBgD2KpxJP98epnAuqhFI9zah/2VJ58WehMMDsytCMFFWA5vSATNnHS2cT
MEKIEkpXT26LMGEoMh7Dj46eiENfzTNe8TwZOVazGfUvPP1d4EXEAG5uA1Apobzr
CSgbK75NG/wz2eYiJQfyZ6sTqURh5dxu9kB9GUOXYVqDAt2JIn6l
-----END RSA PRIVATE KEY-----
";
