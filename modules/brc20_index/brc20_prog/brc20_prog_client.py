# pip install python-dotenv

import os
import requests
from typing import Dict

brc20_prog_enabled = os.getenv("BRC20_PROG_ENABLED") or "false"
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


def initialise(
    genesis_hash: str,
    genesis_timestamp: int,
):
    print("Initialising BRC20PROG")

    result = jsonrpc_call(
        "brc20_initialise",
        params={
            "genesis_hash": genesis_hash,
            "genesis_timestamp": genesis_timestamp,
        },
    )

    return "error" not in result


def add_tx_to_block(
    from_pkscript: str,
    contract_address: str,
    data: str,
    timestamp: int,
    block_hash: str,
    tx_idx: int,
) -> str | None:
    print("Adding transaction to BRC20PROG")

    if contract_address is None:
        tx_result = jsonrpc_call(
            "brc20_addTxToBlock",
            params={
                "from_pkscript": from_pkscript,
                "data": data,
                "timestamp": timestamp,
                "hash": block_hash,
                "tx_idx": tx_idx,
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
                "tx_idx": tx_idx,
            },
        )
    if "error" in tx_result:
        print(tx_result["error"])
        return None
    return tx_result["result"]["contractAddress"]


def finalise_block(block_hash: str, timestamp: int, block_tx_count: int):
    print("Finalising block on BRC20PROG")

    result = jsonrpc_call(
        "brc20_finaliseBlock",
        params={
            "hash": block_hash,
            "timestamp": timestamp,
            "block_tx_cnt": block_tx_count,
        },
    )

    return "error" not in result


def get_block_height():
    return int(jsonrpc_call("eth_blockNumber", {})["result"], 0)


def reorg(block_height):
    result = jsonrpc_call("brc20_reorg", {"latest_valid_block_number": block_height})
    return "error" not in result


def clear_caches():
    result = jsonrpc_call("brc20_clearCaches", {})
    return "error" not in result


def commit_to_database():
    result = jsonrpc_call("brc20_commitToDatabase", {})
    return "error" not in result


if __name__ == "__main__":
    if brc20_prog_enabled == "true":
        print("BRC20 Prog enabled")
    else:
        print("BRC20 Prog disabled")

    print("BRC20 RPC URL: " + brc20_prog_rpc_url)
    print("Block height: " + str(get_block_height()))
