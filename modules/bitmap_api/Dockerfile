FROM node:21-bookworm-slim
RUN apt-get update && \
    apt-get install -y libssl3

WORKDIR /app

COPY package*.json ./
RUN npm install
COPY . .

CMD ["node", "api.js"]