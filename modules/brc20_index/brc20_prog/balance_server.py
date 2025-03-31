import codecs
from typing import Callable
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs
import os
import threading

brc20_prog_balance_url = os.getenv("BRC20_PROG_BALANCE_URL") or "http://localhost:18546"


class BalanceHandler(BaseHTTPRequestHandler):
    def __init__(
        self,
        brc20_balance_function: Callable[[str, str], int],
        address,
        handler,
        server,
    ):
        self.brc20_balance_function = brc20_balance_function
        super().__init__(address, handler, server)

    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.end_headers()
        params = parse_qs(urlparse(self.path).query)
        if "pkscript" not in params or "ticker" not in params:
            self.wfile.write(b"Missing parameters")
            return
        if len(params["pkscript"]) != 1 or len(params["ticker"]) != 1:
            self.wfile.write(b"Invalid parameters")
            return
        if not all(
            isinstance(param, str) and len(param) > 0
            for param in params["pkscript"] + params["ticker"]
        ):
            self.wfile.write(b"Invalid parameters")
            return

        pkscript = params["pkscript"][0].lower()
        if pkscript.startswith("0x"):
            pkscript = pkscript[2:]

        ticker = params["ticker"][0].lower()
        if ticker.startswith("0x"):
            ticker = ticker[2:]

        try:
            # Confirm pkscript is a valid hex string
            codecs.decode(pkscript, "hex")
        except Exception:
            self.wfile.write(b"Invalid pkscript")
            return

        try:
            ticker = codecs.decode(ticker, "hex").decode("utf-8")
        except Exception:
            self.wfile.write(b"Invalid ticker")
            return

        self.wfile.write(
            str(self.brc20_balance_function(pkscript, ticker)).encode("utf-8")
        )


class BRC20BalanceServer:
    def __init__(self, brc20_balance_function: Callable[[str, str], int]):
        self.brc20_balance_function = brc20_balance_function
        self.server = None
        self.server_thread = None

    def start(self):
        global server, server_thread
        server_address = urlparse(brc20_prog_balance_url)
        server = HTTPServer(
            (server_address.hostname, server_address.port),
            lambda address, handler, server: BalanceHandler(
                self.brc20_balance_function, address, handler, server
            ),
        )

        server_thread = threading.Thread(target=server.serve_forever)
        server_thread.start()
        print("BRC20 balance server started.")

    def stop(self):
        if self.server is None:
            return
        self.server.shutdown()
        self.server.server_close()
        self.server_thread.join()
        self.server = None
        self.server_thread = None
        print("BRC20 balance server stopped.")


if __name__ == "__main__":
    balance_server = BRC20BalanceServer(
        lambda pkscript, ticker: abs(hash(pkscript + ticker)) % 100
    )
    balance_server.start()
    input("Press enter to stop server\n")
    balance_server.stop()
