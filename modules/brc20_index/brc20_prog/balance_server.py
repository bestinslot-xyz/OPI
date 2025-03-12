from typing import Callable
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs
import os
import threading
from dotenv import load_dotenv

load_dotenv()
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
        self.wfile.write(
            str(
                self.brc20_balance_function(params["address"][0], params["ticker"][0])
            ).encode("utf-8")
        )


server: HTTPServer = None
server_thread: threading.Thread = None


def start_server(brc20_balance_function: Callable[[str, str], int]):
    global server, server_thread
    server_address = urlparse(brc20_prog_balance_url)
    server = HTTPServer(
        (server_address.hostname, server_address.port),
        lambda address, handler, server: BalanceHandler(
            brc20_balance_function, address, handler, server
        ),
    )

    server_thread = threading.Thread(target=server.serve_forever)
    server_thread.start()
    print("BRC20 balance server started.")


def stop_server():
    global server, server_thread

    server.shutdown()
    server.server_close()
    server_thread.join()
    print("BRC20 balance server stopped.")


if __name__ == "__main__":
    start_server(lambda address, ticker: abs(hash(address + ticker)) % 100)
    input("Press enter to stop server\n")
    stop_server()
