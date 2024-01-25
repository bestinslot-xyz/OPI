# Detailed Installation Guide for OPI on Windows 11 

## Files to download & install before starting:

A. PostgreSQL https://www.postgresql.org/
    ~ Install & name it 'postgres' w/ a pw of your choosing
B. nodes.js https://nodejs.org/en/download
C. python3 https://www.python.org/downloads/
D. Rust https://www.rust-lang.org/ 
    ~ Use Visual Code Installer then open .exe again
E. Git https://git-scm.com/download/win

## Download BTC Core & OPI

1. Open https://bitcoincore.org/ like any other .exe file, go thru install steps. 
~If you are trying to save to an external HD (D-drive), use the GUI of BTC Core save & direct the save to your external drive.~
~For indexing, you'll need to move your %APPDATA% folder to the D-drive (or preferred drive)~

2. Download ZIP file from https://github.com/bestinslot-xyz/OPI & extract to Desktop

```bash
git clone https://github.com/bestinslot-xyz/OPI.git
```

3. Open file explorer on PC with OPI files, copy & paste into Bitcoin/Daemon folder

3.a
~ In file explorer, navigate to 'This PC', 'C-Drive', 'Program files', 'Bitcoin', 'Daemon'.
   ~ Copy the file path *looks like a search bar with the folders you've opened* 
~ Go to search, type in 'env' then click on 'Edit the system enviroment variables'
~ Click 'Enviroment Variables' button near bottom, double-click on 'Path' in bottom window labeled 'System Variables'
~ Click 'New', paste in file path, press ok & continue ok out of panels

## Run BTC Core 

Open 'Command Prompt' (CMD) & run
```bash
cd C:
cd program files
cd bitcoin
cd daemon
bitcoind -txindex
```

**Check blockcount in new CMD**
```bash
bitcoin-cli getblockcount
```
When output matches mempool.space, BTC Core synced.

## Install python libraries

```bash
py -m pip install python-dotenv
py -m pip install pyscopg2-binary
py -m pip install json5
py -m pip install stdiomask
py -m pip install requests
```

## Build Ord

Open 'Command Prompt' (CMD) & run

```bash
cd C:
cd program files
cd bitcoin
cd daemon
cd ord
cargo build --release
```

## Save .env files & enter specifics

~ Open file explorer, navigate to main_index folder
~ Rename '.env_sample' to '.env'
~ Open .env file 
~ Input database info (postgres, etc.)
~ Complete the same process in the following folders:
    ~ bitmap_api, bitmap_index, brc20_api, brc20_index (enter name of index here)

## Install Node.js packages

Open command prompt
```bash
cd C:
cd program files
cd bitcoin
cd daemon
cd modules
cd main_index
npm install
```

Next:
```bash
cd ..
cd brc20_api
npm install
```

Then install:
```bash
cd ..
cd bitmap_api
npm install
```

Then install:
```bash
cd ..
cd sns_api
npm install
```

# Run

**Main Meta-Protocol Indexer**
```bash
cd C:
cd program files
cd bitcoin
cd daemon
cd modules
cd main_index
py reset_init.py
```
~ Complete prompts (if you did .env earlier, 'n' for 1st question)
```bash
node index.js
```

**BRC-20 Indexer**
```bash
cd ..
cd brc20_index
py brc20_index.py
```

**BRC-20 API**
```bash
cd ..
cd brc20_api
node api.js
```

**Bitmap Indexer**
```bash
cd ..
cd bitmap_index
py reset_init.py
```
~ Complete prompts (if you did .env earlier, skip)
```bash
py bitmap_index.py
```

**Bitmap API**
```bash
cd ..
cd bitmap_api
node api.js
```

**SNS Indexer**
```bash
cd ..
cd sns_index
py reset_init.py
```
~ Complete prompts (if you did .env earlier, skip)
```bash
py sns_index.py
```

**SNS API**
```bash
cd ..
cd sns_api
node api.js
```

# Update

- Stop all indexers and apis (preferably starting from main indexer but actually the order shouldn't matter)
- Update the repo (`git pull`)
- Recompile ord 
```bash
cd C: 
cd bitcoin
cd daemon
cd ord 
cargo clean
cargo build --release
```
- Re-run all indexers and apis

