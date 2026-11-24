FROM python:3.12-slim-bookworm@sha256:935c3c5b12357a7e10caa9c1e6ca5692344d97cf229832ef584be2cec9fc03df

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    HOME=/tmp

RUN pip install --no-cache-dir duckdb==1.2.1

WORKDIR /app
COPY explore.py /app/explore.py
RUN chmod 0755 /app/explore.py

USER 65532:65532
ENTRYPOINT ["python3", "/app/explore.py"]
