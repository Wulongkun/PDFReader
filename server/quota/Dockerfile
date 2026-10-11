FROM python:3.12-slim

WORKDIR /app

COPY requirements.txt .
# 服务器直连 PyPI 常间歇性「from versions: none」，走阿里云镜像保证构建稳定。
RUN pip install --no-cache-dir -i https://mirrors.aliyun.com/pypi/simple/ -r requirements.txt

COPY app.py .

EXPOSE 8000

CMD ["uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8000"]
