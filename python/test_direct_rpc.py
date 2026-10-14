"""
Test script for direct HTTP/2 StreamGenerate RPC against Gemini 4 Argon.
Loads session credentials from ~/.config/dragoman/session.json.
"""

import json
import os
import urllib.parse
import httpx

def test_rpc():
    path = os.path.expanduser("~/.config/dragoman/session.json")
    if not os.path.exists(path):
        print(f"Session file not found at {path}. Run 'dragoman login' first.")
        return

    with open(path) as f:
        session = json.load(f)

    bl = session["bl"]
    f_sid = session["f_sid"]
    snlm0e = session["snlm0e"]
    cookie_str = session["cookie"]

    url = f"https://gemini.google.com/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate?bl={bl}&f.sid={f_sid}&hl=en&_reqid=123456&rt=c"

    # Verified 99-element payload structure for Gemini 4 Argon
    inner = [None] * 99
    inner[0] = ["Direct HTTP RPC verification test. Reply with: Direct HTTP confirmed.", 0, None, None, None, None, 0]
    inner[1] = ["en"]
    inner[6] = [1]
    inner[7] = 1
    inner[10] = 1
    inner[11] = 0
    inner[17] = [[2]]  # Enforces Argon (Argon4p0 / 4 Argon mode)
    inner[18] = 0
    inner[27] = 1
    inner[30] = [4, 16, 19]
    inner[41] = [1]
    inner[49] = 0
    inner[53] = 0
    inner[61] = []
    inner[67] = 0
    inner[68] = 2
    inner[79] = 8
    inner[80] = 1
    inner[91] = 1
    inner[96] = 0
    inner[98] = 1

    f_req = json.dumps([None, json.dumps(inner)])

    headers = {
        "User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36",
        "Content-Type": "application/x-www-form-urlencoded;charset=UTF-8",
        "X-Same-Domain": "1",
        "Origin": "https://gemini.google.com",
        "Referer": "https://gemini.google.com/",
        "Cookie": cookie_str
    }

    data = {
        "f.req": f_req,
        "at": snlm0e
    }

    print("Sending Direct HTTP POST to Gemini Web StreamGenerate...")
    with httpx.Client(timeout=60.0) as client:
        resp = client.post(url, headers=headers, data=data)
        print("Status code:", resp.status_code)
        print("Body length:", len(resp.text))
        print("Response sample (first 500 bytes):")
        print(resp.text[:500])

if __name__ == "__main__":
    test_rpc()
