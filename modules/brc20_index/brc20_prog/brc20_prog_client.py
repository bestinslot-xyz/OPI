# pip install python-dotenv

import os
import requests
from typing import Dict

brc20_prog_enabled = (os.getenv("BRC20_PROG_ENABLED") or "false") == "true"
brc20_prog_rpc_url = os.getenv("BRC20_PROG_RPC_URL") or "http://localhost:18545"


def jsonrpc_call(method: str, params: Dict):
    response = requests.post(
        brc20_prog_rpc_url,
        json={
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": "brc20_index",
        },
    )
    return response.json()


class BRC20ProgClient:
    def __init__(self):
        self.current_block_hash = ""
        self.current_block_timestamp = 0
        self.current_block_tx_idx = 0

    def verify_block_hash_and_timestamp(self, block_hash: str, timestamp: int):
        if self.current_block_hash == "" and self.current_block_timestamp == 0:
            self.current_block_hash = block_hash
            self.current_block_timestamp = timestamp
            self.current_block_tx_idx = 0
        elif self.current_block_hash != block_hash:
            raise Exception("Block hash mismatch")
        elif self.current_block_timestamp != timestamp:
            raise Exception("Block timestamp mismatch")

    def initialise(
        self,
        genesis_hash: str,
        genesis_timestamp: int,
        genesis_height: int,
    ):
        if not brc20_prog_enabled:
            return
        print("Initialising BRC20PROG")

        result = jsonrpc_call(
            "brc20_initialise",
            params={
                "genesis_hash": genesis_hash,
                "genesis_timestamp": genesis_timestamp,
                "genesis_height": genesis_height,
            },
        )

        if "error" in result:
            raise Exception(result["error"])

        self.commit_to_database()

    def deposit(
        self,
        to_pkscript: str,
        ticker: str,
        timestamp: int,
        block_hash: str,
        amount: int,
    ) -> bool:
        if not brc20_prog_enabled:
            return False
        self.verify_block_hash_and_timestamp(block_hash, timestamp)

        print("Depositing to BRC20PROG")

        result = jsonrpc_call(
            "brc20_deposit",
            params={
                "to_pkscript": to_pkscript,
                "ticker": ticker,
                "timestamp": timestamp,
                "hash": block_hash,
                "tx_idx": self.current_block_tx_idx,
                "amount": str(amount),
            },
        )

        if "error" in result:
            raise Exception(result["error"])

        self.current_block_tx_idx += 1
        return bool(result["result"]["status"] == "0x1")

    def withdraw(
        self,
        from_pkscript: str,
        ticker: str,
        timestamp: int,
        block_hash: str,
        amount: int,
    ) -> bool:
        if not brc20_prog_enabled:
            return False
        self.verify_block_hash_and_timestamp(block_hash, timestamp)
        print("Withdrawing from BRC20PROG")

        result = jsonrpc_call(
            "brc20_withdraw",
            params={
                "from_pkscript": from_pkscript,
                "ticker": ticker,
                "timestamp": timestamp,
                "hash": block_hash,
                "tx_idx": self.current_block_tx_idx,
                "amount": str(amount),
            },
        )

        if "error" in result:
            raise Exception(result["error"])

        self.current_block_tx_idx += 1
        return bool(result["result"]["status"] == "0x1")

    def add_tx_to_block(
        self,
        from_pkscript: str,
        contract_address: str,
        data: str,
        timestamp: int,
        block_hash: str,
    ) -> str:
        if not brc20_prog_enabled:
            return
        self.verify_block_hash_and_timestamp(block_hash, timestamp)
        print("Adding transaction to BRC20PROG")

        if contract_address is None:
            tx_result = jsonrpc_call(
                "brc20_addTxToBlock",
                params={
                    "from_pkscript": from_pkscript,
                    "data": data,
                    "timestamp": timestamp,
                    "hash": block_hash,
                    "tx_idx": self.current_block_tx_idx,
                },
            )
        else:
            tx_result = jsonrpc_call(
                "brc20_addTxToBlock",
                params={
                    "from_pkscript": from_pkscript,
                    "to": contract_address,
                    "data": data,
                    "timestamp": timestamp,
                    "hash": block_hash,
                    "tx_idx": self.current_block_tx_idx,
                },
            )

        if "error" in tx_result:
            raise Exception(tx_result["error"])

        self.current_block_tx_idx += 1
        if tx_result["result"]["contractAddress"] is None:
            return tx_result["result"]["status"] == "0x1"
        tx_result["result"]["contractAddress"]

    def mine_blocks(self, block_count: int):
        if not brc20_prog_enabled:
            return
        result = jsonrpc_call("brc20_mine", {"block_count": block_count, "timestamp": 0})
        if "error" in result:
            raise Exception(result["error"])

        self.reset_current_block()

    def finalise_block(self, block_hash: str, timestamp: int):
        if not brc20_prog_enabled:
            return
        self.verify_block_hash_and_timestamp(block_hash, timestamp)
        print("Finalising block on BRC20PROG")

        result = jsonrpc_call(
            "brc20_finaliseBlock",
            params={
                "hash": block_hash,
                "timestamp": timestamp,
                "block_tx_count": self.current_block_tx_idx,
            },
        )

        if "error" in result:
            raise Exception(result["error"])

        self.reset_current_block()

    def get_block_hash(self, block_height: int):
        if not brc20_prog_enabled:
            return ""
        result = jsonrpc_call("eth_getBlockByNumber", {"block": str(block_height)})
        if "error" in result:
            raise Exception(result["error"])

        return result["result"]["hash"]

    def get_block_height(self):
        if not brc20_prog_enabled:
            return 0
        return int(jsonrpc_call("eth_blockNumber", {})["result"], 0)

    def reorg(self, block_height):
        if not brc20_prog_enabled:
            return
        result = jsonrpc_call(
            "brc20_reorg", {"latest_valid_block_number": block_height}
        )
        if "error" in result:
            raise Exception(result["error"])

        self.reset_current_block()

    def clear_caches(self):
        if not brc20_prog_enabled:
            return
        result = jsonrpc_call("brc20_clearCaches", {})
        if "error" in result:
            raise Exception(result["error"])

        self.reset_current_block()

    def commit_to_database(self):
        if not brc20_prog_enabled:
            return
        result = jsonrpc_call("brc20_commitToDatabase", {})
        if "error" in result:
            raise Exception(result["error"])

    def reset_current_block(self):
        self.current_block_hash = ""
        self.current_block_timestamp = 0
        self.current_block_tx_idx = 0


if __name__ == "__main__":
    if brc20_prog_enabled == "true":
        print("BRC20 Prog enabled")
    else:
        print("BRC20 Prog disabled")

    client = BRC20ProgClient()
    print("BRC20 RPC URL: " + brc20_prog_rpc_url)
    print("Block height: " + str(client.get_block_height()))
