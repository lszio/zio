FROM nginx:alpine
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY site /usr/share/nginx/html/site
COPY docs /usr/share/nginx/html/docs
COPY examples /usr/share/nginx/html/examples
COPY book /usr/share/nginx/html/book
COPY blog /usr/share/nginx/html/blog
