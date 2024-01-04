# pip install python-dotenv
# pip install psycopg2-binary

import os, psycopg2, pathlib
from dotenv import load_dotenv

res = input("Are you sure you want to reset the metaprotocol database? (y/n) ")
if res != 'y':
  print('aborting')
  exit(1)

load_dotenv()
db_user = os.getenv("DB_USER") or "postgres"
db_host = os.getenv("DB_HOST") or "localhost"
db_port = int(os.getenv("DB_PORT") or "5432")
db_database = os.getenv("DB_DATABASE") or "postgres"
db_password = os.getenv("DB_PASSWD")

## connect to db
conn = psycopg2.connect(
  host=db_host,
  port=db_port,
  database=db_database,
  user=db_user,
  password=db_password)
conn.autocommit = True
cur = conn.cursor()

## reset db
sqls = open('db_reset.sql', 'r').read().split(';')
for sql in sqls:
  if sql.strip() != '':
    cur.execute(sql)

sqls = open('db_init.sql', 'r').read().split(';')
for sql in sqls:
  if sql.strip() != '':
    cur.execute(sql)

## close db
cur.close()
conn.close()

ord_folder = os.getenv("ORD_FOLDER") or "../../ord/target/release/"
ord_datadir = os.getenv("ORD_DATADIR") or "."

ord_folder = pathlib.Path(ord_folder).absolute()
ord_datadir = pathlib.Path(ord_folder, ord_datadir).absolute()

ord_index_redb_path = pathlib.Path(ord_datadir, "index.redb").absolute()
ord_index_redb_path.unlink(missing_ok=True)

ord_log_file_path = pathlib.Path(ord_folder, "log_file.txt").absolute()
ord_log_file_path.write_text("")

ord_log_file_index_path = pathlib.Path(ord_folder, "log_file_index.txt").absolute()
ord_log_file_index_path.write_text("")

log_file_error_path = pathlib.Path("log_file_error.txt").absolute()
log_file_error_path.write_text("")

print('done')