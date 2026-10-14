import asyncio
import json
import websockets

async def inspect_menu():
    ws_url = "ws://127.0.0.1:9222/devtools/page/F1E9A0908DA749E63F607A76B95A9CDC"
    async with websockets.connect(ws_url) as ws:
        await ws.send(json.dumps({"id": 1, "method": "Runtime.enable", "params": {}}))
        await ws.recv()

        # Click the model picker button
        click_js = """
        (async function() {
            const pickerBtn = document.querySelector('button[aria-label*="mode picker"], button[aria-label*="model picker"]') ||
                              Array.from(document.querySelectorAll('button')).find(b => (b.innerText || '').includes('Flash'));
            if (!pickerBtn) return { error: "Picker button not found" };

            pickerBtn.click();
            await new Promise(r => setTimeout(r, 600));

            // Query menu items
            const menuItems = Array.from(document.querySelectorAll('[role="menuitem"], [role="option"], mat-option, .mat-mdc-menu-item, [role="menu"] button'));
            return {
                clicked: true,
                pickerText: pickerBtn.innerText,
                menuItems: menuItems.map(m => ({
                    text: m.innerText.trim(),
                    ariaLabel: m.getAttribute('aria-label'),
                    role: m.getAttribute('role'),
                    classes: m.className
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

asyncio.run(inspect_menu())
