## takes around 25 mins with pbzip2, depends on network speed, disk speed, ram and cpu

# pip install python-dotenv
# pip install psycopg2-binary
# pip install boto3
# pip install tqdm

## apt install postgresql-client-common
## apt install postgresql-client-14
## apt install pbzip2

import os, psycopg2, sys
from dotenv import dotenv_values
import pathlib

import boto3
from botocore import UNSIGNED
from botocore.client import Config

from tqdm import tqdm

def get_yn(question, default=None):
    if not sys.stdin.isatty():
        return default
    while True:
        res = input(question + " (y/n): ")
        if res.lower() == 'y':
            return True
        elif res.lower() == 'n':
            return False
        else:
            print("Invalid input")

index_brc20 = get_yn("Will you index brc20", True)
index_bitmap = get_yn("Will you index bitmap", True)
index_sns = get_yn("Will you index sns", True)

if not os.path.isfile('main_index/.env'):
  print("main_index/.env file not found, please run reset_init.py from main_index folder")
  exit()

if index_brc20 and not os.path.isfile('brc20_index/.env'):
  print("brc20_index/.env file not found, please run reset_init.py from brc20_index folder")
  exit()

if index_bitmap and not os.path.isfile('bitmap_index/.env'):
  print("bitmap_index/.env file not found, please run reset_init.py from bitmap_index folder")
  exit()

if index_sns and not os.path.isfile('sns_index/.env'):
  print("sns_index/.env file not found, please run reset_init.py from sns_index folder")
  exit()

env = dotenv_values(dotenv_path='main_index/.env')
db_user_main = env.get("DB_USER") or "postgres"
db_host_main = env.get("DB_HOST") or "localhost"
db_port_main = int(env.get("DB_PORT") or "5432")
db_database_main = env.get("DB_DATABASE") or "postgres"
db_password_main = env.get("DB_PASSWD")

try:
  conn_main = psycopg2.connect(
    host=db_host_main,
    port=db_port_main,
    database=db_database_main,
    user=db_user_main,
    password=db_password_main)
  conn_main.autocommit = True
  cur_main = conn_main.cursor()
  cur_main.close()
  conn_main.close()
except:
  print("Error connecting to main db, check main_index/.env file")
  exit()

ord_folder = env.get("ORD_FOLDER") or "../../ord/target/release/"
ord_datadir = env.get("ORD_DATADIR") or "."
ord_binary = env.get("ORD_BINARY") or "ord"
if not pathlib.Path("main_indexer", ord_folder, ord_binary).resolve().exists():
  print("ord binary not found, please check ORD_FOLDER and ORD_BINARY in main_index/.env file, and make sure you have built ord using 'cargo build --release'")
  exit()

if not pathlib.Path("main_indexer", ord_folder, ord_datadir).resolve().exists():
  print("ord datadir not found, please check ORD_FOLDER and ORD_DATADIR in main_index/.env file")
  exit()

if index_brc20:
  env = dotenv_values(dotenv_path='brc20_index/.env')
  db_user_brc20 = env.get("DB_USER") or "postgres"
  db_host_brc20 = env.get("DB_HOST") or "localhost"
  db_port_brc20 = int(env.get("DB_PORT") or "5432")
  db_database_brc20 = env.get("DB_DATABASE") or "postgres"
  db_password_brc20 = env.get("DB_PASSWD")

  try:
    conn_brc20 = psycopg2.connect(
      host=db_host_brc20,
      port=db_port_brc20,
      database=db_database_brc20,
      user=db_user_brc20,
      password=db_password_brc20)
    conn_brc20.autocommit = True
    cur_brc20 = conn_brc20.cursor()
    cur_brc20.close()
    conn_brc20.close()
  except:
    print("Error connecting to brc20 db, check brc20_index/.env file")
    exit()

if index_bitmap:
  env = dotenv_values(dotenv_path='bitmap_index/.env')
  db_user_bitmap = env.get("DB_USER") or "postgres"
  db_host_bitmap = env.get("DB_HOST") or "localhost"
  db_port_bitmap = int(env.get("DB_PORT") or "5432")
  db_database_bitmap = env.get("DB_DATABASE") or "postgres"
  db_password_bitmap = env.get("DB_PASSWD")

  try:
    conn_bitmap = psycopg2.connect(
      host=db_host_bitmap,
      port=db_port_bitmap,
      database=db_database_bitmap,
      user=db_user_bitmap,
      password=db_password_bitmap)
    conn_bitmap.autocommit = True
    cur_bitmap = conn_bitmap.cursor()
    cur_bitmap.close()
    conn_bitmap.close()
  except:
    print("Error connecting to bitmap db, check bitmap_index/.env file")
    exit()

if index_sns:
  env = dotenv_values(dotenv_path='sns_index/.env')
  db_user_sns = env.get("DB_USER") or "postgres"
  db_host_sns = env.get("DB_HOST") or "localhost"
  db_port_sns = int(env.get("DB_PORT") or "5432")
  db_database_sns = env.get("DB_DATABASE") or "postgres"
  db_password_sns = env.get("DB_PASSWD")

  try:
    conn_sns = psycopg2.connect(
      host=db_host_sns,
      port=db_port_sns,
      database=db_database_sns,
      user=db_user_sns,
      password=db_password_sns)
    conn_sns.autocommit = True
    cur_sns = conn_sns.cursor()
    cur_sns.close()
    conn_sns.close()
  except:
    print("Error connecting to sns db, check sns_index/.env file")
    exit()

download_only = not get_yn("Do you want to restore databases (y) or download backups only (n)?", True)

restore_index_redb = get_yn("Do you want to restore index.redb?", True)
if not download_only and restore_index_redb:
  res = os.system('tar --help >/dev/null 2>&1')
  if res != 0:
    print("tar is not installed, cannot restore index.redb, you may use download_only option and extract yourself")
    exit()
  res = os.system('pbzip2 -V >/dev/null 2>&1')
  if res != 0:
    res = get_yn("pbzip2 is not installed, will use normal tar, may take around 40 mins with normal tar, it'll take around 5 mins with pbzip2. Do you want to continue?", False)
    if not res:
      exit()
restore_main_db = get_yn("Do you want to restore main db?", True)
restore_brc20_db = False
if index_brc20: restore_brc20_db = get_yn("Do you want to restore brc20 db?", True)
restore_bitmap_db = False
if index_bitmap: restore_bitmap_db = get_yn("Do you want to restore bitmap db?", True)
restore_sns_db = False
if index_sns: restore_sns_db = get_yn("Do you want to restore sns db?", True)
if not download_only and (restore_main_db or restore_brc20_db or restore_bitmap_db or restore_sns_db):
  res = os.system('pg_restore -V >/dev/null 2>&1')
  if res != 0:
    print("pg_restore is not installed, cannot restore databases, you may use download_only option and restore yourself")
    exit()


OBJECT_STORAGE_BUCKET = 'opi-backups'

s3config = {
  "endpoint_url": "http://s3.opi.network:9000",
  "aws_session_token": None,
  "verify": False
}

s3client = boto3.client('s3', **s3config, config=Config(signature_version=UNSIGNED))
def get_backup_filenames():
  list_files = s3client.list_objects(Bucket=OBJECT_STORAGE_BUCKET)['Contents']
  res = []
  for key in list_files:
    res.append(key['Key'])
  return res

S3_KEY_PREFIX = 'db_5/'
def s3_download(s3_bucket, s3_object_key, local_file_name):
  s3_object_key = S3_KEY_PREFIX + s3_object_key
  meta_data = s3client.head_object(Bucket=s3_bucket, Key=s3_object_key)
  total_length = int(meta_data.get('ContentLength', 0))
  with tqdm(total=total_length,  desc=f'source: s3://{s3_bucket}/{s3_object_key}', bar_format="{percentage:.1f}%|{bar:25} | {rate_fmt} | {desc}",  unit='B', unit_scale=True, unit_divisor=1024) as pbar:
    with open(local_file_name, 'wb') as f:
      s3client.download_fileobj(s3_bucket, s3_object_key, f, Callback=pbar.update)

backup_filenames = get_backup_filenames()
index_backup_heights = []
main_backup_heights = []
brc20_backup_heights = []
bitmap_backup_heights = []
sns_backup_heights = []
for filename in backup_filenames:
  if filename.startswith(S3_KEY_PREFIX + 'index_') and filename.endswith('.redb.tar.bz2'):
    index_backup_heights.append(int(filename.split('.')[0].split('_')[-1]))
  elif filename.startswith(S3_KEY_PREFIX + 'postgres_metaprotocol_') and filename.endswith('.dump'):
    main_backup_heights.append(int(filename.split('.')[0].split('_')[-1]))
  elif filename.startswith(S3_KEY_PREFIX + 'postgres_brc20_') and filename.endswith('.dump'):
    brc20_backup_heights.append(int(filename.split('.')[0].split('_')[-1]))
  elif filename.startswith(S3_KEY_PREFIX + 'postgres_bitmap_') and filename.endswith('.dump'):
    bitmap_backup_heights.append(int(filename.split('.')[0].split('_')[-1]))
  elif filename.startswith(S3_KEY_PREFIX + 'postgres_sns_') and filename.endswith('.dump'):
    sns_backup_heights.append(int(filename.split('.')[0].split('_')[-1]))

found_heights = None
if restore_index_redb:
  if found_heights is None:
    found_heights = index_backup_heights
  else:
    found_heights = list(set(found_heights).intersection(index_backup_heights))

if restore_main_db:
  if found_heights is None:
    found_heights = main_backup_heights
  else:
    found_heights = list(set(found_heights).intersection(main_backup_heights))

if restore_brc20_db:
  if found_heights is None:
    found_heights = brc20_backup_heights
  else:
    found_heights = list(set(found_heights).intersection(brc20_backup_heights))

if restore_bitmap_db:
  if found_heights is None:
    found_heights = bitmap_backup_heights
  else:
    found_heights = list(set(found_heights).intersection(bitmap_backup_heights))

if restore_sns_db:
  if found_heights is None:
    found_heights = sns_backup_heights
  else:
    found_heights = list(set(found_heights).intersection(sns_backup_heights))

if found_heights is None or len(found_heights) == 0:
  print("No backups found")
  exit()

max_found_height = max(found_heights)
print("Found backups for height: " + str(max_found_height))

if restore_index_redb:
  print("Restoring index.redb")
  if download_only:
    print("Downloading index.redb.tar.bz2")
    s3_download(OBJECT_STORAGE_BUCKET, "index_" + str(max_found_height) + ".redb.tar.bz2", "index.redb.tar.bz2")
  else:
    print("Removing old files")
    ord_index_redb_path = pathlib.Path("main_indexer", ord_folder, ord_datadir, "index.redb").resolve()
    ord_index_redb_path.unlink(missing_ok=True)
    ord_log_file_path = pathlib.Path("main_indexer", ord_folder, "log_file.txt").resolve()
    ord_log_file_path.write_text("")
    ord_log_file_index_path = pathlib.Path("main_indexer", ord_folder, "log_file_index.txt").resolve()
    ord_log_file_index_path.write_text("")
    log_file_error_path = pathlib.Path("main_index", "log_file_error.txt").resolve()
    log_file_error_path.write_text("")
    print("Downloading index.redb.tar.bz2")
    path = pathlib.Path("main_indexer", ord_folder, ord_datadir, "index.redb.tar.bz2").resolve()
    s3_download(OBJECT_STORAGE_BUCKET, "index_" + str(max_found_height) + ".redb.tar.bz2", path)
    current_directory = os.getcwd()
    path = pathlib.Path("main_indexer", ord_folder, ord_datadir).resolve()
    os.chdir(path)
    print("Extracting index.redb.tar.bz2 this may take a while (~30 mins)")
    res = os.system('pbzip2 -V >/dev/null 2>&1')
    if res != 0:
      print("pbzip2 is not installed, using normal tar, may take longer")
      res = os.system("tar xjf index.redb.tar.bz2")
      if res != 0:
        print("Error extracting index.redb")
        exit()
    else:
      res = os.system("tar xf index.redb.tar.bz2 --use-compress-prog=pbzip2")
      if res != 0:
        print("Error extracting index.redb")
        exit()
    os.unlink("index.redb.tar.bz2")
    os.chdir(current_directory)

if restore_main_db:
  print("Restoring main db")
  print("Downloading postgres_metaprotocol.dump")
  s3_download(OBJECT_STORAGE_BUCKET, "postgres_metaprotocol_" + str(max_found_height) + ".dump", "postgres_metaprotocol.dump")
  if not download_only:
    print("Restoring postgres_metaprotocol.dump")
    conn_main = psycopg2.connect(
      host=db_host_main,
      port=db_port_main,
      database=db_database_main,
      user=db_user_main,
      password=db_password_main)
    conn_main.autocommit = True
    cur_main = conn_main.cursor()
    sqls = open('main_index/db_reset.sql', 'r').read().split(';')
    for sql in sqls:
      if sql.strip() != '':
        cur_main.execute(sql)
    cur_main.close()
    conn_main.close()
    os.environ["PGPASSWORD"]='{}'.format(db_password_main)
    res = os.system("pg_restore --no-owner --jobs=4 -U " + db_user_main + " -Fc -c --if-exists -v -d " + db_database_main + " -h " + db_host_main + " -p " + str(db_port_main) + " postgres_metaprotocol.dump")
    if res != 0:
      print("Error restoring main db")
      exit()
    os.unlink("postgres_metaprotocol.dump")

if restore_brc20_db:
  print("Restoring brc20 db")
  print("Downloading postgres_brc20.dump")
  s3_download(OBJECT_STORAGE_BUCKET, "postgres_brc20_" + str(max_found_height) + ".dump", "postgres_brc20.dump")
  if not download_only:
    print("Restoring postgres_brc20.dump")
    conn_brc20 = psycopg2.connect(
      host=db_host_brc20,
      port=db_port_brc20,
      database=db_database_brc20,
      user=db_user_brc20,
      password=db_password_brc20)
    conn_brc20.autocommit = True
    cur_brc20 = conn_brc20.cursor()
    sqls = open('brc20_index/db_reset.sql', 'r').read().split(';')
    for sql in sqls:
      if sql.strip() != '':
        cur_brc20.execute(sql)
    sqls = open('brc20_index/db_reset_extra.sql', 'r').read().split(';')
    for sql in sqls:
      if sql.strip() != '':
        cur_brc20.execute(sql)
    cur_brc20.close()
    conn_brc20.close()
    os.environ["PGPASSWORD"]='{}'.format(db_password_brc20)
    res = os.system("pg_restore --no-owner --jobs=4 -U " + db_user_brc20 + " -Fc -c --if-exists -v -d " + db_database_brc20 + " -h " + db_host_brc20 + " -p " + str(db_port_brc20) + " postgres_brc20.dump")
    if res != 0:
      print("Error restoring brc20 db")
      exit()
    os.unlink("postgres_brc20.dump")

if restore_bitmap_db:
  print("Restoring bitmap db")
  print("Downloading postgres_bitmap.dump")
  s3_download(OBJECT_STORAGE_BUCKET, "postgres_bitmap_" + str(max_found_height) + ".dump", "postgres_bitmap.dump")
  if not download_only:
    print("Restoring postgres_bitmap.dump")
    conn_bitmap = psycopg2.connect(
      host=db_host_bitmap,
      port=db_port_bitmap,
      database=db_database_bitmap,
      user=db_user_bitmap,
      password=db_password_bitmap)
    conn_bitmap.autocommit = True
    cur_bitmap = conn_bitmap.cursor()
    sqls = open('bitmap_index/db_reset.sql', 'r').read().split(';')
    for sql in sqls:
      if sql.strip() != '':
        cur_bitmap.execute(sql)
    cur_bitmap.close()
    conn_bitmap.close()
    os.environ["PGPASSWORD"]='{}'.format(db_password_bitmap)
    res = os.system("pg_restore --no-owner --jobs=4 -U " + db_user_bitmap + " -Fc -c --if-exists -v -d " + db_database_bitmap + " -h " + db_host_bitmap + " -p " + str(db_port_bitmap) + " postgres_bitmap.dump")
    if res != 0:
      print("Error restoring bitmap db")
      exit()
    os.unlink("postgres_bitmap.dump")

if restore_sns_db:
  print("Restoring sns db")
  print("Downloading postgres_sns.dump")
  s3_download(OBJECT_STORAGE_BUCKET, "postgres_sns_" + str(max_found_height) + ".dump", "postgres_sns.dump")
  if not download_only:
    print("Restoring postgres_sns.dump")
    conn_sns = psycopg2.connect(
      host=db_host_sns,
      port=db_port_sns,
      database=db_database_sns,
      user=db_user_sns,
      password=db_password_sns)
    conn_sns.autocommit = True
    cur_sns = conn_sns.cursor()
    sqls = open('sns_index/db_reset.sql', 'r').read().split(';')
    for sql in sqls:
      if sql.strip() != '':
        cur_sns.execute(sql)
    cur_sns.close()
    conn_sns.close()
    os.environ["PGPASSWORD"]='{}'.format(db_password_sns)
    res = os.system("pg_restore --no-owner --jobs=4 -U " + db_user_sns + " -Fc -c --if-exists -v -d " + db_database_sns + " -h " + db_host_sns + " -p " + str(db_port_sns) + " postgres_sns.dump")
    if res != 0:
      print("Error restoring sns db")
      exit()
    os.unlink("postgres_sns.dump")

print("Done")
