project = "nginx-demo"

service "nginx" {
  host = "nginx.unisrv.dev"
}

deployment "nginx" {
  service = "nginx"
  port    = 80
  image   = "nginx:latest"
}