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

async def inspect_menu():
    ws_url = get_gemini_tab_ws()
    async with websockets.connect(ws_url) as ws:
        await ws.send(json.dumps({"id": 1, "method": "Runtime.enable", "params": {}}))
        await ws.recv()

        click_js = """
        (async function() {
            const pickerBtn = document.querySelector('button[aria-label*="mode picker"], button[aria-label*="model picker"]') ||
                              Array.from(document.querySelectorAll('button')).find(b => (b.innerText || '').includes('Flash'));
            if (!pickerBtn) return { error: "Picker button not found" };

            pickerBtn.click();
            await new Promise(r => setTimeout(r, 600));

            const menuItems = Array.from(document.querySelectorAll('[role="menuitem"], [role="option"], mat-option, .mat-mdc-menu-item, [role="menu"] button'));
            return {
                clicked: true,
                pickerText: pickerBtn.innerText,
                items: menuItems.map(m => ({
                    text: (m.innerText || '').trim(),
                    ariaLabel: m.getAttribute('aria-label'),
                    role: m.getAttribute('role'),
                    className: m.className
                }))
            };
        })();
        """

        req = {
            "id": 2,
            "method": "Runtime.evaluate",
            "params": {
                "expression": click_js,
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
    asyncio.run(inspect_menu())
