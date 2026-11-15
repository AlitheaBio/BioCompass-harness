FROM python:3.12-slim-bookworm

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    HOME=/tmp \
    PATH="/opt/venv/bin:$PATH"

RUN python -m venv /opt/venv \
    && /opt/venv/bin/pip install --no-cache-dir duckdb==1.2.1 \
    && mkdir -p /app /in /out \
    && chown -R 65532:65532 /out

COPY --chmod=0555 analyze.py /app/analyze.py

USER 65532:65532
WORKDIR /app
ENTRYPOINT ["/opt/venv/bin/python", "/app/analyze.py"]
