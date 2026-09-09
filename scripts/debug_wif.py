#!/usr/bin/env python3
"""TEMPORARY debug: faithful STS exchange (camelCase JSON, exactly like
google-github-actions/auth wif.ts) + JWT decode to reveal the EXACT
WIF principal subject. Delete after the IAM bindings are pinned."""
import base64, json, os, urllib.request, urllib.parse, urllib.error

def dec(seg):
    seg += "=" * (-len(seg) % 4)
    return base64.urlsafe_b64decode(seg).decode()

def b64u(s):
    return base64.urlsafe_b64encode(s.encode()).rstrip(b"=").decode()

gh = os.environ["GHA_ID_TOKEN"]
print("=== GITHUB OIDC TOKEN CLAIMS (subject_token) ===")
h, p, _ = gh.split(".")
claims = json.loads(dec(p))
for k in ("iss", "sub", "aud", "repository", "repositoryOwner", "ref", "job"):
    print(f"  {k}: {claims.get(k)}")

NUM = "153176493081"
aud = f"//iam.googleapis.com/projects/{NUM}/locations/global/workloadIdentityPools/pocerbanting/providers/github"

def sts(tok_type):
    body = {
        "audience": aud,
        "grantType": "urn:ietf:params:oauth:grant-type:token-exchange",
        "requestedTokenType": tok_type,
        "scope": "https://www.googleapis.com/auth/cloud-platform",
        "subjectTokenType": "urn:ietf:params:oauth:token-type:jwt",
        "subjectToken": gh,
    }
    data = json.dumps(body).encode()
    req = urllib.request.Request(
        "https://sts.googleapis.com/v1/token", data=data,
        headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return {"error": e.read().decode()[:500], "code": e.code}

print("\n=== STS exchange -> WIF PRINCIPAL JWT ===")
out = sts("urn:ietf:params:oauth:token-type:jwt")
if "error" in out:
    print("  STS ERROR:", out["code"], out["error"])
else:
    tok = out.get("issuedToken", "")
    parts = tok.split(".")
    if len(parts) >= 2:
        for k, v in json.loads(dec(parts[1])).items():
            print(f"  {k}: {v}")
    else:
        print("  issuedToken not a JWT:", tok[:120])
