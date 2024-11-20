FROM node:21-bookworm-slim
RUN apt-get update && \
    apt-get install -y libssl3

WORKDIR /app

COPY ord /usr/local/bin/ord
RUN chmod +x /usr/local/bin/ord
COPY package*.json ./
RUN npm install
COPY . .

CMD ["node", "index_runes.js"]