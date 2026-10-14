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

async def trace():
    ws_url = get_gemini_tab_ws()
    async with websockets.connect(ws_url, max_size=20*1024*1024) as ws:
        await ws.send(json.dumps({"id": 1, "method": "Network.enable", "params": {}}))
        await ws.send(json.dumps({"id": 2, "method": "Runtime.enable", "params": {}}))

        select_argon_js = """
        (async function() {
            let menuItems = Array.from(document.querySelectorAll('[role="menuitem"], .mat-mdc-menu-item'));
            if (menuItems.length === 0) {
                const pickerBtn = document.querySelector('button[aria-label*="mode picker"], button[aria-label*="model picker"]') ||
                                  Array.from(document.querySelectorAll('button')).find(b => /flash|pro|argon/i.test(b.innerText || ''));
                if (pickerBtn) pickerBtn.click();
                await new Promise(r => setTimeout(r, 500));
                menuItems = Array.from(document.querySelectorAll('[role="menuitem"], .mat-mdc-menu-item'));
            }

            const argonItem = menuItems.find(m => (m.innerText || '').includes('4 Argon'));
            if (argonItem) {
                argonItem.click();
                return "CLICKED_ARGON";
            }
            return "NOT_FOUND";
        })();
        """

        await ws.send(json.dumps({
            "id": 3,
            "method": "Runtime.evaluate",
            "params": {"expression": select_argon_js, "returnByValue": True, "awaitPromise": True}
        }))

        print("Listening for Network events...")
        while True:
            msg = await ws.recv()
            data = json.loads(msg)
            method = data.get("method")

            if method == "Network.requestWillBeSent":
                req = data["params"]["request"]
                url = req.get("url", "")
                if "BardChatUi" in url or "StreamGenerate" in url:
                    print(f"\n[REQUEST] {req.get('method')} {url}")
                    post_data = req.get("postData")
                    if post_data:
                        print(f"POST DATA (first 300 chars): {post_data[:300]}")

if __name__ == "__main__":
    asyncio.run(trace())
