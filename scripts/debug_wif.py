#!/usr/bin/env python3
"""TEMPORARY debug (one-shot):
1. Mint the real GitHub ID token via the RUNNER's own endpoint vars
   (ACTIONS_ID_TOKEN_REQUEST_URL/TOKEN) — no more guessing URLs.
2. Print its sub claim verbatim.
3. Perform the official STS exchange (camelCase, full provider audience).
4. Decode the minted WIF principal JWT -> its exact subject + attribute.repository.
5. Try getAccessToken on the SA as the WIF principal -> the real impersonation test.
Delete with debug-wif.yml when CI is green.
"""
import base64, json, os, sys, urllib.request, urllib.error, urllib.parse

NUM = "153176493081"
POOL = "pocerbanting"
AUD = f"//iam.googleapis.com/projects/{NUM}/locations/global/workloadIdentityPools/{POOL}/providers/github"
SA = "pocerbanting-ci@clear-region-377216.iam.gserviceaccount.com"


def b64url_decode(seg):
    return base64.urlsafe_b64decode(seg + "=" * (-len(seg) % 4))


def jwt_payload(jwt):
    return json.loads(b64url_decode(jwt.split(".")[1]))


def post_json(url, body, headers=None):
    h = {"Content-Type": "application/json"}
    h.update(headers or {})
    req = urllib.request.Request(url, data=json.dumps(body).encode(), headers=h, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode() or "{}")


# 1) real ID token from the runner's own endpoint
req_url = os.environ["ACTIONS_ID_TOKEN_REQUEST_URL"]
req_tok = os.environ["ACTIONS_ID_TOKEN_REQUEST_TOKEN"]
if "audience" not in req_url:
    req_url += "&audience=" + AUD
else:
    req_url = req_url.split("audience=")[0] + "audience=" + AUD
oidc_req = urllib.request.Request(
    req_url,
    headers={"Authorization": f"bearer {req_tok}", "Accept": "application/json; api-version=1.0"},
)
try:
    with urllib.request.urlopen(oidc_req, timeout=30) as r:
        id_token = json.loads(r.read().decode())["value"]
except Exception as e:
    print(f"FATAL: could not mint ID token: {e}")
    sys.exit(1)

p = jwt_payload(id_token)
print("=== GITHUB ID TOKEN claims (the assertion STS validates) ===")
for k in ("iss", "sub", "aud", "repository", "repository_owner", "ref", "ref_type", "event_name"):
    print(f"  {k}: {p.get(k)}")

# 2) official STS exchange (identical body to google-github-actions/auth wif.ts)
body = {
    "audience": AUD,
    "grantType": "urn:ietf:params:oauth:grant-type:token-exchange",
    "requestedTokenType": "urn:ietf:params:oauth:token-type:access_token",
    "scope": "https://www.googleapis.com/auth/cloud-platform",
    "subjectTokenType": "urn:ietf:params:oauth:token-type:jwt",
    "subjectToken": id_token,
}
status, resp = post_json("https://sts.googleapis.com/v1/token", body)
if status >= 300:
    print(f"=== STS ERROR: HTTP {status} ===")
    print(json.dumps(resp, indent=1)[:800])
    sys.exit(1)

principal = resp["access_token"]
print("\n=== MINTED WIF PRINCIPAL (decoded) ===")
try:
    cp = jwt_payload(principal)
    for k, v in cp.items():
        print(f"  {k}: {v}")
    print("\n>>> EXACT PRINCIPAL TO BIND:")
    print(f"    principal://iam.googleapis.com/projects/{NUM}/locations/global/"
          f"workloadIdentityPools/{POOL}/subject/{cp.get('sub')}")
    repo = (cp.get("attribute") or {}).get("repository")
    print(f">>> attribute.repository = {repo!r}")
except Exception as ex:
    print("  (decode failed)", ex)
    sys.exit(1)

# 3) REAL impersonation test: getAccessToken as the WIF principal
url = f"https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/{SA}:generateAccessToken"
status2, resp2 = post_json(url, {"scope": ["cloud-platform"]},
                           headers={"Authorization": f"Bearer {principal}"})
print(f"\n=== GETACCTOKEN AS WIF PRINCIPAL: HTTP {status2} ===")
if status2 < 300:
    print("  IMPERSONATION WORKS. token len:", len(resp2.get("accessToken", "")))
else:
    msg = (resp2.get("error") or {}).get("message", json.dumps(resp2)[:400])
    print("  STILL DENIED:", msg[:500])
