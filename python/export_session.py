import asyncio
import json
import os
import websockets

async def export_session():
    ws_url = "ws://127.0.0.1:9222/devtools/page/F1E9A0908DA749E63F607A76B95A9CDC"
    async with websockets.connect(ws_url) as ws:
        # 1. Get WIZ_global_data
        await ws.send(json.dumps({"id": 1, "method": "Runtime.evaluate", "params": {
            "expression": "JSON.stringify({SNlM0e: window.WIZ_global_data?.SNlM0e, f_sid: window.WIZ_global_data?.FdrFJe, bl: window.WIZ_global_data?.cfb2h})",
            "returnByValue": True
        }}))
        m1 = json.loads(await ws.recv())
        wiz = json.loads(m1["result"]["result"]["value"])

        # 2. Get Cookies
        await ws.send(json.dumps({"id": 2, "method": "Network.getCookies", "params": {
            "urls": ["https://gemini.google.com", "https://google.com"]
        }}))
        m2 = json.loads(await ws.recv())
        cookies_list = m2.get("result", {}).get("cookies", [])
        cookie_header = "; ".join([c["name"] + "=" + c["value"] for c in cookies_list])

        session = {
            "snlm0e": wiz.get("SNlM0e"),
            "f_sid": wiz.get("f_sid"),
            "bl": wiz.get("bl"),
            "cookie": cookie_header,
            "cookies": {c["name"]: c["value"] for c in cookies_list},
            "model": "Argon4p0"
        }

        config_dir = os.path.expanduser("~/.config/dragoman")
        os.makedirs(config_dir, exist_ok=True)
        path = os.path.join(config_dir, "session.json")
        with open(path, "w") as f:
            json.dump(session, f, indent=2)

        print("Session successfully exported to", path)
        print("SNlM0e:", session["snlm0e"][:10] + "...")
        print("BL:", session["bl"])
        print("Cookies count:", len(cookies_list))

asyncio.run(export_session())
