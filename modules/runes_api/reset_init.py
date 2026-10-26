# pip install python-dotenv

import os

init_env = True

# does .env file exist?
if os.path.isfile('.env'):
  res = input("Do you want to re-initialise .env file? (y/n) ")
  if res != 'y':
    init_env = False

if init_env:
  DB_USER="postgres"
  DB_HOST="localhost"
  DB_PORT="5432"
  DB_DATABASE="postgres"
  DB_PASSWD=""
  DB_SSL="true"
  DB_MAX_CONNECTIONS=10
  API_HOST="127.0.0.1"
  API_PORT="8003"
  API_TRUSTED_PROXY_CNT="0"
  print("Initialising .env file")
  print("leave blank to use default values")
  res = input("Runes Postgres DB username (Default: postgres): ")
  if res != '':
    DB_USER = res
  res = input("Runes Postgres DB host (Default: localhost) leave default if postgres is installed on the same machine: ")
  if res != '':
    DB_HOST = res
  res = input("Runes Postgres DB port (Default: 5432): ")
  if res != '':
    DB_PORT = res
  res = input("Runes Postgres DB name (Default: postgres) leave default if no new dbs are created: ")
  if res != '':
    DB_DATABASE = res
  res = input("Runes Postgres DB password: ")
  DB_PASSWD = res
  res = input("Runes Postgres DB use SSL (Default: true) may need to be set to false on Windows machines: ")
  if res != '':
    DB_SSL = res
  res = input("Runes Postgres DB max connections (Default: 10): ")
  if res != '':
    DB_MAX_CONNECTIONS = res
  res = input("API host (Default: 127.0.0.1): ")
  if res != '':
    API_HOST = res
  res = input("API port (Default: 8003): ")
  if res != '':
    API_PORT = res
  res = input("API trusted proxy count (Default: 0) if there are known proxies such as nginx in front of API, set this to the number of proxies: ")
  if res != '':
    API_TRUSTED_PROXY_CNT = res
  f = open('.env', 'w')
  f.write("DB_USER=\"" + DB_USER + "\"\n")
  f.write("DB_HOST=\"" + DB_HOST + "\"\n")
  f.write("DB_PORT=\"" + str(DB_PORT) + "\"\n")
  f.write("DB_DATABASE=\"" + DB_DATABASE + "\"\n")
  f.write("DB_PASSWD=\"" + DB_PASSWD + "\"\n")
  f.write("DB_SSL=\"" + DB_SSL + "\"\n")
  f.write("DB_MAX_CONNECTIONS=" + str(DB_MAX_CONNECTIONS) + "\n")
  f.write("API_HOST=\"" + API_HOST + "\"\n")
  f.write("API_PORT=\"" + API_PORT + "\"\n")
  f.write("API_TRUSTED_PROXY_CNT=\"" + API_TRUSTED_PROXY_CNT + "\"\n")
  f.close()
