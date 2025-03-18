openssl req -x509 -nodes -newkey rsa:4096 \
    -keyout scheduler_key.pem \
    -out scheduler_cert.pem \
    -days 365 \
    -config san.cnf \
    -extensions v3_req
