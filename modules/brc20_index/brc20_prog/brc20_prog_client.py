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

    if "error" in result:
        raise Exception(result["error"])

    commit_to_database()


def deposit(
    to_pkscript: str,
    ticker: str,
    timestamp: int,
    block_hash: str,
    tx_idx: int,
    amount: int,
) -> bool:
    print("Depositing to BRC20PROG")

    result = jsonrpc_call(
        "brc20_deposit",
        params={
            "to_pkscript": to_pkscript,
            "ticker": ticker,
            "timestamp": timestamp,
            "hash": block_hash,
            "tx_idx": tx_idx,
            "amount": str(amount),
        },
    )

    if "error" in result:
        raise Exception(result["error"])
    return bool(result["result"])


def withdraw(
    from_pkscript: str,
    ticker: str,
    timestamp: int,
    block_hash: str,
    tx_idx: int,
    amount: int,
) -> bool:
    print("Withdrawing from BRC20PROG")

    result = jsonrpc_call(
        "brc20_withdraw",
        params={
            "from_pkscript": from_pkscript,
            "ticker": ticker,
            "timestamp": timestamp,
            "hash": block_hash,
            "tx_idx": tx_idx,
            "amount": str(amount),
        },
    )

    print(result)

    if "error" in result:
        raise Exception(result["error"])
    return bool(result["result"])


def add_tx_to_block(
    from_pkscript: str,
    contract_address: str,
    data: str,
    timestamp: int,
    block_hash: str,
    tx_idx: int,
) -> str:
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
        raise Exception(tx_result["error"])
    if tx_result["result"]["contractAddress"] is None:
        return tx_result["result"]["status"] == "0x1"
    tx_result["result"]["contractAddress"]


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

    if "error" in result:
        raise Exception(result["error"])


def get_block_height():
    return int(jsonrpc_call("eth_blockNumber", {})["result"], 0)


def reorg(block_height):
    result = jsonrpc_call("brc20_reorg", {"latest_valid_block_number": block_height})
    if "error" in result:
        raise Exception(result["error"])


def clear_caches():
    result = jsonrpc_call("brc20_clearCaches", {})
    if "error" in result:
        raise Exception(result["error"])


def commit_to_database():
    result = jsonrpc_call("brc20_commitToDatabase", {})
    if "error" in result:
        raise Exception(result["error"])


if __name__ == "__main__":
    if brc20_prog_enabled == "true":
        print("BRC20 Prog enabled")
    else:
        print("BRC20 Prog disabled")

    print("BRC20 RPC URL: " + brc20_prog_rpc_url)
    print("Block height: " + str(get_block_height()))
