# Host API v1 GitHub observations. External data, without actions or I/O.
# ctx.state starts empty per callback. Setters target only this hook's session.
GithubContext = record(provider = str, cwd = str, state = dict[str, typing.Any])
