# pip install -r ../requirements.txt
#
# Writes the API settings into the repository root .env, the file opi-indexer
# also reads. Other settings already in that file are kept.

import os
from dotenv import dotenv_values, set_key
import stdiomask

ENV_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '.env')

MODULES = ['brc20', 'bitmap', 'sns', 'runes']
# brc20 keeps the unprefixed DB_* variables
DB_PREFIX = {'brc20': 'DB', 'bitmap': 'BITMAP_DB', 'sns': 'SNS_DB', 'runes': 'RUNES_DB'}

existing = dotenv_values(dotenv_path=ENV_PATH) if os.path.isfile(ENV_PATH) else {}

if existing.get("OPI_MODULES") or existing.get("API_PORT"):
  res = input("API settings already exist in .env, do you want to re-initialise them? (y/n) ")
  if res != 'y':
    raise SystemExit(0)

def ask(prompt, default):
  res = input(prompt + " (Default: " + str(default) + "): ")
  return res if res != '' else str(default)

print("Initialising API settings in " + os.path.normpath(ENV_PATH))
print("leave blank to use default values")

while True:
  modules_value = ask("Modules to serve, comma-separated, any of brc20, bitmap, sns, runes", existing.get("OPI_MODULES") or "brc20")
  modules = []
  for name in modules_value.split(','):
    name = name.strip()
    if name != '' and name not in modules:
      modules.append(name)
  unknown = [name for name in modules if name not in MODULES]
  if len(modules) > 0 and len(unknown) == 0:
    break
  print("OPI_MODULES must name at least one of: " + ", ".join(MODULES))

settings = {"OPI_MODULES": ",".join(modules)}

for module in modules:
  prefix = DB_PREFIX[module]
  label = module.upper() + " Postgres DB"
  settings[prefix + "_USER"] = ask(label + " username", existing.get(prefix + "_USER") or "postgres")
  settings[prefix + "_HOST"] = ask(label + " host, leave default if postgres is installed on the same machine", existing.get(prefix + "_HOST") or "localhost")
  settings[prefix + "_PORT"] = ask(label + " port", existing.get(prefix + "_PORT") or "5432")
  while True:
    database = ask(label + " name (required)", existing.get(prefix + "_DATABASE") or "")
    if database != '':
      break
  settings[prefix + "_DATABASE"] = database
  settings[prefix + "_PASSWD"] = existing.get(prefix + "_PASSWD") or ""
  res = stdiomask.getpass(label + " password (blank keeps the current one): ")
  if res != '':
    settings[prefix + "_PASSWD"] = res
  settings[prefix + "_SSL"] = ask(label + " use SSL, may need to be false on Windows machines", existing.get(prefix + "_SSL") or "true")

settings["DB_MAX_CONNECTIONS"] = ask("Max connections per module database", existing.get("DB_MAX_CONNECTIONS") or "10")
settings["API_HOST"] = ask("API host", existing.get("API_HOST") or "127.0.0.1")
settings["API_PORT"] = ask("API port", existing.get("API_PORT") or "8000")
settings["API_TRUSTED_PROXY_CNT"] = ask("API trusted proxy count, if there are known proxies such as nginx in front of API, set this to the number of proxies", existing.get("API_TRUSTED_PROXY_CNT") or "0")
settings["RATE_LIMIT_ENABLE"] = ask("Enable the API rate limiter (true/false)", existing.get("RATE_LIMIT_ENABLE") or "false")
if settings["RATE_LIMIT_ENABLE"] == 'true':
  settings["RATE_LIMIT_WINDOW_MS"] = ask("Rate limit window in ms, 15 minutes is 900000", existing.get("RATE_LIMIT_WINDOW_MS") or "900000")
  settings["RATE_LIMIT_MAX"] = ask("Rate limit max requests per window", existing.get("RATE_LIMIT_MAX") or "100")

if not os.path.isfile(ENV_PATH):
  open(ENV_PATH, 'a').close()
for key, value in settings.items():
  set_key(ENV_PATH, key, value, quote_mode='never' if key == 'DB_MAX_CONNECTIONS' else 'always')
