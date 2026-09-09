#!/usr/bin/env python3
"""TEMPORARY debug: perform the WIF STS exchange and decode the actual WIF
principal subject claim so the IAM `subject/` bindings can be pinned exactly.
Delete with debug-wif.yml once the CI deploys go green."""
import base64, json, os, urllib.request, urllib.parse, urllib.error

oidc = os.environ["OIDC"]
AUD = ("//iam.googleapis.com/projects/153176493081/locations/global/"
       "workloadIdentityPools/pocerbanting/providers/github")

def b64j(s):
    s = s + "=" * (-len(s) % 4)
    return json.loads(base64.urlsafe_b64decode(s))

print("===== GITHUB OIDC (input) claim: sub =====")
c = b64j(oidc.split(".")[1])
print("  github.sub =", c.get("sub"))
print("  repository =", c.get("repository"), "owner =", c.get("repository_owner"))

# Request a JWT so we can decode the WIF principal subject.
data = urllib.parse.urlencode({
    "grant_type": "urn:ietf:params:oauth:grant-type:token-exchange",
    "subject_token": oidc,
    "subject_token_type": "urn:ietf:params:oauth:token-type:id_token",
    "audience": AUD,
    "requested_token_type": "urn:ietf:params:oauth:token-type:jwt",
}).encode()
req = urllib.request.Request("https://sts.googleapis.com/v1/token", data=data,
                             method="POST")
try:
    resp = json.load(urllib.request.urlopen(req, timeout=30))
except urllib.error.HTTPError as e:
    resp = json.load(e)
if "error" in resp:
    print("  STS ERROR:", json.dumps(resp))
    raise SystemExit(0)

t = resp.get("id_token") or resp.get("access_token") or ""
print("===== WIF PRINCIPAL TOKEN (decoded) =====")
parts = t.split(".")
if len(parts) == 3:
    wc = b64j(parts[1])
    for k in sorted(wc):
        print(f"  {k}: {wc[k]}")
    print("  >>> WIF PRINCIPAL SUBJECT (must match IAM subject/ binding):")
    print("       " + str(wc.get("sub")))
else:
    print("  (not a 3-part JWT; keys returned:", list(resp.keys()), ")")
