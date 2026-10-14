import asyncio
import json
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

async def inspect():
    ws_url = get_gemini_tab_ws()
    async with websockets.connect(ws_url) as ws:
        await ws.send(json.dumps({"id": 1, "method": "Runtime.enable", "params": {}}))
        await ws.recv()

        js_code = """
        (function() {
            const accountEl = document.querySelector('a[aria-label*="Google Account"], img[alt*="Google Account"], [data-ogb-account-name]');
            const accountText = accountEl ? (accountEl.getAttribute('aria-label') || accountEl.getAttribute('alt') || '') : '';

            const allButtons = Array.from(document.querySelectorAll('button, [role="button"], [role="combobox"]'));
            const modelCandidates = allButtons.filter(b => {
                const t = (b.innerText || '') + (b.getAttribute('aria-label') || '');
                return /gemini|model|argon|flash|pro|advanced|ultra/i.test(t);
            }).map(b => ({
                text: (b.innerText || '').trim(),
                ariaLabel: b.getAttribute('aria-label'),
                role: b.getAttribute('role'),
                tag: b.tagName
            }));

            return {
                title: document.title,
                url: window.location.href,
                account: accountText,
                modelCandidates: modelCandidates.slice(0, 15)
            };
        })();
        """

        req = {
            "id": 2,
            "method": "Runtime.evaluate",
            "params": {
                "expression": js_code,
                "returnByValue": True,
                "awaitPromise": True
            }
        }
        await ws.send(json.dumps(req))

        while True:
            msg = await ws.recv()
            data = json.loads(msg)
            if data.get("id") == 2:
                print(json.dumps(data.get("result", {}).get("result", {}).get("value", {}), indent=2))
                break

if __name__ == "__main__":
    asyncio.run(inspect())
