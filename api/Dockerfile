FROM node:22-alpine
WORKDIR /app
COPY package*.json ./
RUN npm ci --omit=dev
COPY src ./src
COPY bin ./bin
ENV DATA_DIR=/data HOST=0.0.0.0 PORT=8080
VOLUME /data
EXPOSE 8080
USER node
CMD ["node", "src/server.js"]
