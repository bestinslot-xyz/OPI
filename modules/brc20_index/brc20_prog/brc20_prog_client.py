# pip install python-dotenv

import os
import requests
from requests import auth
from typing import Dict

brc20_prog_enabled = (os.getenv("BRC20_PROG_ENABLED") or "false") == "true"
brc20_prog_rpc_url = os.getenv("BRC20_PROG_RPC_URL") or "http://localhost:18545"
network_type = os.getenv("NETWORK_TYPE") or "mainnet"

brc20_prog_auth = auth.HTTPBasicAuth(
    os.getenv("BRC20_PROG_RPC_SERVER_USER") or "user",
    os.getenv("BRC20_PROG_RPC_SERVER_PASSWORD") or "password",
)

"""
  The first block height where BRC20 Prog inscriptions can be added to the blockchain
  This affects brc20_deploy, brc20_call, brc20_deposit and brc20_withdraw
  If the block height is less than this value, the BRC20 Prog client will not add transactions
  This is to prevent inscriptions from being added to the blockchain before the BRC20 Prog module is initialised

  TODO: Update this value to the correct block height before launching on a network
"""
brc20_prog_first_inscription_heights = {
    "mainnet": 767431,  # TODO: Update this value to the correct block height before launching on mainnet
    "testnet": 2413344,  # TODO: Update this value to the correct block height before launching on testnet
    "testnet4": 2,  # TODO: Update this value to the correct block height before launching on testnet4
    "signet": 230000,  # TODO: Update this value to the correct block height before launching on signet
    "regtest": 2,  # TODO: Update this value to the correct block height before launching on regtest
}


def check_brc20_prog_enabled():
    global brc20_prog_enabled
    return brc20_prog_enabled


def check_brc20_prog_inscriptions_enabled(current_block_height):
    global brc20_prog_enabled, network_type, brc20_prog_first_inscription_heights
    return (
        brc20_prog_enabled
        and current_block_height >= brc20_prog_first_inscription_heights[network_type]
    )


def jsonrpc_call(method: str, params: Dict):
    response = requests.post(
        brc20_prog_rpc_url,
        auth=brc20_prog_auth,
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
        self.reset_current_block()

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
        if not check_brc20_prog_enabled():
            return
        print("Initialising BRC20PROG")

        if not genesis_hash.startswith("0x"):
            genesis_hash = "0x" + genesis_hash

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

    def is_enabled(self):
        return check_brc20_prog_enabled()

    def version(self):
        result = jsonrpc_call("brc20_version", {})
        if "error" in result:
            raise Exception(result["error"])
        return result["result"]

    def deposit(
        self,
        to_pkscript: str,
        ticker: str,
        timestamp: int,
        block_hash: str,
        amount: int,
        inscription_id: str,
    ):  # Returns TxReceipt value
        if not check_brc20_prog_inscriptions_enabled(self.current_block_height):
            return {}
        self.verify_block_hash_and_timestamp(block_hash, timestamp)

        print("Depositing to BRC20PROG")

        if not block_hash.startswith("0x"):
            block_hash = "0x" + block_hash

        result = jsonrpc_call(
            "brc20_deposit",
            params={
                "to_pkscript": to_pkscript,
                "ticker": ticker,
                "timestamp": timestamp,
                "hash": block_hash,
                "tx_idx": self.current_block_tx_idx,
                "amount": str(amount),
                "inscription_id": inscription_id,
            },
        )

        if "error" in result:
            raise Exception(result["error"])

        self.current_block_tx_idx += 1
        return result["result"]

    def withdraw(
        self,
        from_pkscript: str,
        ticker: str,
        timestamp: int,
        block_hash: str,
        amount: int,
        inscription_id: str,
    ):  # Returns TxReceipt value
        if not check_brc20_prog_inscriptions_enabled(self.current_block_height):
            return {}
        self.verify_block_hash_and_timestamp(block_hash, timestamp)
        print("Withdrawing from BRC20PROG")

        if not block_hash.startswith("0x"):
            block_hash = "0x" + block_hash

        result = jsonrpc_call(
            "brc20_withdraw",
            params={
                "from_pkscript": from_pkscript,
                "ticker": ticker,
                "timestamp": timestamp,
                "hash": block_hash,
                "tx_idx": self.current_block_tx_idx,
                "amount": str(amount),
                "inscription_id": inscription_id,
            },
        )

        if "error" in result:
            raise Exception(result["error"])

        self.current_block_tx_idx += 1
        return result["result"]

    def deploy(
        self,
        from_pkscript: str,
        data: str,
        timestamp: int,
        block_hash: str,
        inscription_id: str,
        inscription_byte_len: int,
    ) -> str:
        if not brc20_prog_enabled:
            return
        self.verify_block_hash_and_timestamp(block_hash, timestamp)
        print("Adding transaction to BRC20PROG")

        if not block_hash.startswith("0x"):
            block_hash = "0x" + block_hash

        tx_result = jsonrpc_call(
            "brc20_deploy",
            params={
                "from_pkscript": from_pkscript,
                "data": data,
                "timestamp": timestamp,
                "hash": block_hash,
                "tx_idx": self.current_block_tx_idx,
                "inscription_id": inscription_id,
                "inscription_byte_len": inscription_byte_len,
            },
        )

        if "error" in tx_result:
            raise Exception(tx_result["error"])

        if tx_result["result"]["status"] == "0x0":
            print("Transaction failed")
            print(tx_result)

        self.current_block_tx_idx += 1
        return tx_result["result"]["contractAddress"]

    def call(
        self,
        from_pkscript: str,
        contract_address: str,
        contract_inscription_id: str,
        data: str,
        timestamp: int,
        block_hash: str,
        inscription_id: str,
        inscription_byte_len: int,
    ):
        if not brc20_prog_enabled:
            return
        self.verify_block_hash_and_timestamp(block_hash, timestamp)
        print("Adding transaction to BRC20PROG")

        if not block_hash.startswith("0x"):
            block_hash = "0x" + block_hash

        tx_result = jsonrpc_call(
            "brc20_call",
            params={
                "from_pkscript": from_pkscript,
                "contract_address": contract_address,
                "contract_inscription_id": contract_inscription_id,
                "data": data,
                "timestamp": timestamp,
                "hash": block_hash,
                "tx_idx": self.current_block_tx_idx,
                "inscription_id": inscription_id,
                "inscription_byte_len": inscription_byte_len,
            },
        )

        if "error" in tx_result:
            raise Exception(tx_result["error"])

        if tx_result["result"] is None or tx_result["result"]["status"] == "0x0":
            print("Transaction failed")

        self.current_block_tx_idx += 1
        return tx_result["result"]

    def mine_blocks(self, block_count: int):
        if not check_brc20_prog_enabled():
            return
        result = jsonrpc_call(
            "brc20_mine", {"block_count": block_count, "timestamp": 0}
        )
        if "error" in result:
            raise Exception(result["error"])

        self.reset_current_block()

    def finalise_block(self, block_hash: str, timestamp: int):
        if not check_brc20_prog_enabled():
            return
        self.verify_block_hash_and_timestamp(block_hash, timestamp)
        print("Finalising block on BRC20PROG")

        if not block_hash.startswith("0x"):
            block_hash = "0x" + block_hash

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
        if not check_brc20_prog_enabled():
            return None
        result = jsonrpc_call(
            "eth_getBlockByNumber", {"block": str(block_height), "is_full": False}
        )
        if "error" in result:
            return None

        return result["result"]["hash"]

    def get_block_height(self):
        if not check_brc20_prog_enabled():
            return 0
        return int(jsonrpc_call("eth_blockNumber", {})["result"], 0)

    def reorg(self, block_height):
        if not check_brc20_prog_enabled():
            return
        result = jsonrpc_call(
            "brc20_reorg", {"latest_valid_block_number": block_height}
        )
        if "error" in result:
            raise Exception(result["error"])

        self.reset_current_block()

    def clear_caches(self):
        if not check_brc20_prog_enabled():
            return
        result = jsonrpc_call("brc20_clearCaches", {})
        if "error" in result:
            raise Exception(result["error"])

        self.reset_current_block()

    def commit_to_database(self):
        if not check_brc20_prog_enabled():
            return
        result = jsonrpc_call("brc20_commitToDatabase", {})
        if "error" in result:
            raise Exception(result["error"])

    def reset_current_block(self):
        self.current_block_hash = ""
        self.current_block_timestamp = 0
        self.current_block_tx_idx = 0
        self.current_block_height = self.get_block_height()


if __name__ == "__main__":
    if brc20_prog_enabled == "true":
        print("BRC20 Prog enabled")
    else:
        print("BRC20 Prog disabled")

    client = BRC20ProgClient()
    print("BRC20 RPC URL: " + brc20_prog_rpc_url)
    print("Block height: " + str(client.get_block_height()))
    print("Version: " + client.version())
