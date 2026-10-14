import json
import os
import urllib.parse
import httpx

def test_rpc():
    path = os.path.expanduser("~/.config/dragoman/session.json")
    with open(path) as f:
        session = json.load(f)

    bl = session["bl"]
    f_sid = session["f_sid"]
    snlm0e = session["snlm0e"]
    cookie_str = session["cookie"]

    url = f"https://gemini.google.com/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate?bl={bl}&f.sid={f_sid}&hl=en&_reqid=123456&rt=c"

    # Inner prompt array structure for Gemini
    inner_payload = [
        ["Direct HTTP RPC verification test. Reply with: Direct HTTP confirmed.", 0, None, None, None, None, 0],
        ["en"],
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        [["fbb127bbb056c959", "5bf011840784117a", "9d8ca3786ebdfbea", "Argon4p0"]]
    ]

    f_req = json.dumps([None, json.dumps(inner_payload)])

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
        print("Headers Content-Type:", resp.headers.get("content-type"))
        print("Body length:", len(resp.text))
        print("Response sample (first 500 bytes):")
        print(resp.text[:500])

test_rpc()
