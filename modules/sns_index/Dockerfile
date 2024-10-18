# Python tabanlı bir imaj kullan
FROM python:3.11-slim

# Çalışma dizinini ayarla
WORKDIR /app

RUN apt-get update && apt-get install -y \
    libpq-dev gcc python3-dev build-essential --no-install-recommends && \
    rm -rf /var/lib/apt/lists/*

# Gereksinim dosyasını kopyala ve bağımlılıkları yükle
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

# Uygulama kodunu kopyala
COPY . .

# Uygulamayı çalıştır
CMD ["python", "sns_index.py"]