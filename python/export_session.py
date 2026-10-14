"""
Dragoman session exporter utility.
Connects to Chrome via CDP, queries the active Gemini tab, extracts
WIZ_global_data and cookies, and saves credentials to ~/.config/dragoman/session.json.
"""

import asyncio
import json
import os
import urllib.request
import websockets

def get_gemini_tab_ws(cdp_port: int = 9222) -> str:
    url = f"http://127.0.0.1:{cdp_port}/json"
    with urllib.request.urlopen(url) as resp:
        tabs = json.loads(resp.read().decode())
    for t in tabs:
        if "gemini.google.com" in t.get("url", ""):
            ws_url = t.get("webSocketDebuggerUrl")
            if ws_url:
                return ws_url
    raise RuntimeError("No active Gemini tab found with a WebSocket debugger URL.")

async def export_session():
    ws_url = get_gemini_tab_ws()
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
            "f_sid": str(wiz.get("f_sid")),
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
        print("BL:", session["bl"])
        print("Cookies count:", len(cookies_list))

if __name__ == "__main__":
    asyncio.run(export_session())
