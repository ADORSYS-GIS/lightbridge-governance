#!/usr/bin/env python3
"""A throwaway OIDC issuer for assert-refusal-classification.sh.

Serves `.well-known/openid-configuration` + a JWKS over plain HTTP, backed by
an RSA keypair generated fresh on every run (there is nothing to persist --
this process and its key die with the test). The pinned `oidcauthextension`
image reaches this via `--add-host=host.docker.internal:host-gateway`, so it
must bind 0.0.0.0, not just loopback.

Usage:
    mock_oidc_issuer.py serve <port> <issuer-url> <private-key-out-path>
        Generates a keypair, writes the PEM private key to the given path
        (so `sign` below can mint tokens against the SAME key this process
        serves the JWKS for), and serves forever.

    mock_oidc_issuer.py sign <private-key-path> --iss ISS --aud AUD
        [--exp-offset-seconds N] [--bad-signature]
        Mints one JWT and prints it to stdout. `--exp-offset-seconds` is
        relative to now (negative = already expired). `--bad-signature`
        signs with a SECOND, throwaway keypair instead of the real one --
        this is how the "verification_failed" catch-all case is produced:
        a token whose iss/aud/exp are all otherwise valid, but whose
        signature the real JWKS cannot verify.
"""

import argparse
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

import jwt
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import rsa

KID = "test-key-1"


def _b64url_uint(n: int) -> str:
    length = (n.bit_length() + 7) // 8
    return jwt.utils.base64url_encode(n.to_bytes(length, "big")).decode()


def generate_keypair() -> rsa.RSAPrivateKey:
    return rsa.generate_private_key(public_exponent=65537, key_size=2048)


def jwk_for(private_key: rsa.RSAPrivateKey) -> dict:
    numbers = private_key.public_key().public_numbers()
    return {
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": KID,
        "n": _b64url_uint(numbers.n),
        "e": _b64url_uint(numbers.e),
    }


def cmd_serve(args: argparse.Namespace) -> None:
    private_key = generate_keypair()
    pem = private_key.private_bytes(
        encoding=serialization.Encoding.PEM,
        format=serialization.PrivateFormat.PKCS8,
        encryption_algorithm=serialization.NoEncryption(),
    )
    with open(args.private_key_out, "wb") as f:
        f.write(pem)

    jwks_body = json.dumps({"keys": [jwk_for(private_key)]}).encode()
    discovery_body = json.dumps(
        {
            "issuer": args.issuer_url,
            "jwks_uri": f"{args.issuer_url}/jwks",
            "authorization_endpoint": f"{args.issuer_url}/authorize",
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        }
    ).encode()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == "/.well-known/openid-configuration":
                body = discovery_body
            elif self.path == "/jwks":
                body = jwks_body
            else:
                self.send_response(404)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass  # keep the test's stdout readable

    HTTPServer(("0.0.0.0", args.port), Handler).serve_forever()


def cmd_sign(args: argparse.Namespace) -> None:
    with open(args.private_key_path, "rb") as f:
        real_key = serialization.load_pem_private_key(f.read(), password=None)

    # `--bad-signature`: sign with a DIFFERENT, never-published keypair so the
    # real JWKS cannot verify it -- this is the deliberate "wrong signature"
    # case, distinct from expired/wrong-audience, that should land in the
    # generic `verification_failed` catch-all.
    signing_key = generate_keypair() if args.bad_signature else real_key

    now = int(time.time())
    payload = {
        "iss": args.iss,
        "aud": args.aud,
        "sub": "test-subject",
        "iat": now,
        "exp": now + args.exp_offset_seconds,
    }
    token = jwt.encode(payload, signing_key, algorithm="RS256", headers={"kid": KID})
    print(token)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    p_serve = sub.add_parser("serve")
    p_serve.add_argument("port", type=int)
    p_serve.add_argument("issuer_url")
    p_serve.add_argument("private_key_out")
    p_serve.set_defaults(func=cmd_serve)

    p_sign = sub.add_parser("sign")
    p_sign.add_argument("private_key_path")
    p_sign.add_argument("--iss", required=True)
    p_sign.add_argument("--aud", required=True)
    p_sign.add_argument("--exp-offset-seconds", type=int, default=3600)
    p_sign.add_argument("--bad-signature", action="store_true")
    p_sign.set_defaults(func=cmd_sign)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
