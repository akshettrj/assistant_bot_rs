"""Fetch the local keys of your Tuya devices through a Smart Life QR login.

Tuya devices encrypt their LAN traffic with a per-device "local key" that
only Tuya's cloud hands out. This asks for it once, the same way Home
Assistant's Tuya integration logs in (its public client id), so no Tuya IoT
developer account is needed:

1. find your user code in the Smart Life app: Me -> Settings (gear) ->
   Account and Security -> User Code;
2. run `nix run .#tuya-local-key -- <user code>`;
3. scan the QR code from inside the Smart Life app (+ -> Scan, not the phone
   camera) and confirm. For the Tuya Smart app, pass `--scheme tuyaSmart`.

The devices (id, name, local key, IP, data points) are printed as JSON on
stdout; instructions go to stderr. Nothing is stored. The local key changes
whenever the device is re-paired.
"""

import argparse
import json
import sys
import time

import qrcode
from tuya_sharing import LoginControl, Manager

# Home Assistant's client id and schema for this login flow
# (homeassistant/components/tuya/const.py).
CLIENT_ID = "HA_3y9q4ak7g4ephrvke"
SCHEMA = "haauthorize"
LOGIN_TIMEOUT_SECS = 180


def log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def login(user_code: str, scheme: str) -> dict:
    control = LoginControl()
    response = control.qr_code(CLIENT_ID, SCHEMA, user_code)
    if not response.get("success"):
        sys.exit(f"error: could not start the login: {response.get('msg', response)}")
    token = response["result"]["qrcode"]

    qr = qrcode.QRCode(border=1)
    # The prefix tells which app may scan the code.
    qr.add_data(f"{scheme}--qrLogin?token={token}")
    qr.print_ascii(out=sys.stderr, invert=True)
    app = "Smart Life" if scheme == "smartlife" else "Tuya Smart"
    log(f"Scan this from inside the {app} app (+ -> Scan) and confirm the login.")

    deadline = time.monotonic() + LOGIN_TIMEOUT_SECS
    while time.monotonic() < deadline:
        ok, info = control.login_result(token, CLIENT_ID, user_code)
        if ok:
            return info
        time.sleep(2)
    sys.exit("error: the QR code was not confirmed in time")


def describe(device) -> dict:
    def spec(items: dict) -> dict:
        return {
            code: {"type": item.type, "values": json.loads(item.values or "{}")}
            for code, item in items.items()
        }

    return {
        "id": device.id,
        "name": device.name,
        "local_key": device.local_key,
        "ip": device.ip,
        "online": device.online,
        "category": device.category,
        "product_id": device.product_id,
        "product_name": device.product_name,
        "status": device.status,
        "functions": spec(device.function),
        "status_range": spec(device.status_range),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("user_code", nargs="?", help="Me -> Settings -> Account and Security -> User Code")
    parser.add_argument("--scheme", default="smartlife", choices=["smartlife", "tuyaSmart"],
                        help="the app that will scan the QR code (default: smartlife)")
    args = parser.parse_args()
    user_code = args.user_code or input("User code: ").strip()
    info = login(user_code, args.scheme)

    manager = Manager(CLIENT_ID, user_code, info["terminal_id"], info["endpoint"], info)
    manager.update_device_cache()
    devices = [describe(device) for device in manager.device_map.values()]

    log(f"Found {len(devices)} device(s).")
    print(json.dumps(devices, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
