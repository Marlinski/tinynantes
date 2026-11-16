# Tiny Nantes: static site (player + published tile pyramids), served by unprivileged nginx on :8080.
# Build context = the tarball made by deploy/package.sh (web/ including web/data/, which is not on GitHub).
FROM nginxinc/nginx-unprivileged:1.27-alpine
COPY deploy/nginx.conf /etc/nginx/conf.d/default.conf
COPY web/ /usr/share/nginx/html/
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://127.0.0.1:8080/healthz || exit 1
