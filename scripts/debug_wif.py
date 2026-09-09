#!/usr/bin/env python3
"""TEMPORARY debug: perform the EXACT STS exchange google-github-actions/auth
does (camelCase JSON body, requestedTokenType=access_token), then decode the
returned WIF principal JWT and print every claim — including the real subject
and the repository attribute. Delete with debug-wif.yml after the IAM
bindings are pinned correctly."""
import base64, json, os, sys, urllib.request, urllib.error

AUD = "//iam.googleapis.com/projects/153176493081/locations/global/workloadIdentityPools/pocerbanting/providers/github"
token = os.environ["GHA_ID_TOKEN"]


def b64url_decode(seg):
    pad = "=" * (-len(seg) % 4)
    return base64.urlsafe_b64decode(seg + pad)


def decode_jwt(jwt):
    h, p, s = jwt.split(".")
    hdr = json.loads(b64url_decode(h))
    payload = json.loads(b64url_decode(p))
    return hdr, payload


# 1) GitHub OIDC token claims (the subject_token)
h, p, s = token.split(".")
print("=== GITHUB OIDC TOKEN (subject_token) claims ===")
for k, v in json.loads(b64url_decode(p)).items():
    if k in ("sub", "repository", "repository_owner", "ref", "iss", "aud"):
        print(f"  {k}: {v}")

# 2) Faithful STS exchange — identical body to wif.ts lines 166-173
body = {
    "audience": AUD,
    "grantType": "urn:ietf:params:oauth:grant-type:token-exchange",
    "requestedTokenType": "urn:ietf:params:oauth:token-type:access_token",
    "scope": "https://www.googleapis.com/auth/cloud-platform",
    "subjectTokenType": "urn:ietf:params:oauth:token-type:jwt",
    "subjectToken": token,
}
req = urllib.request.Request(
    "https://sts.googleapis.com/v1/token",
    data=json.dumps(body).encode(),
    headers={"Content-Type": "application/json"},
    method="POST",
)
try:
    with urllib.request.urlopen(req, timeout=30) as r:
        resp = json.loads(r.read().decode())
except urllib.error.HTTPError as e:
    print(f"=== STS ERROR: HTTP {e.code} ===")
    print(e.read().decode()[:500])
    sys.exit(1)

principal = resp.get("access_token", "")
print("\n=== WIF PRINCIPAL TOKEN (decoded JWT) — THE REAL SUBJECT ===")
try:
    hdr, payload = decode_jwt(principal)
    for k, v in payload.items():
        print(f"  {k}: {v}")
    print("\n>>> REAL SUBJECT TO USE IN IAM BINDINGS:")
    print(f"    principal://iam.googleapis.com/projects/153176493081/"
          f"locations/global/workloadIdentityPools/pocerbanting/subject/{payload.get('sub')}")
    attrs = payload.get("attribute", {})
    if attrs:
        print(f">>> principalSet option (attribute.repository):")
        print(f"    principalSet://iam.googleapis.com/projects/153176493081/"
              f"locations/global/workloadIdentityPools/pocerbanting/attribute.repository/{attrs.get('repository')}")
except Exception as ex:
    print("  (not a JWT, or decode failed):", repr(principal[:200]))
