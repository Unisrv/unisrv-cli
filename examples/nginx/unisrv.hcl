project = "nginx"

service "nginx" {
  deployment = "nginx"
}

deployment "nginx" {
  port = 80
  container {
    image = "nginx:latest"
  }
}
