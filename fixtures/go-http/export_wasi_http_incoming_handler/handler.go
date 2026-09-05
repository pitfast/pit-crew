package export_wasi_http_incoming_handler

import (
	"fmt"
	"strings"
	. "go.bytecodealliance.org/pkg/wit/types"
	"wit_component/wasi_http_outgoing_handler"
	. "wit_component/wasi_http_types"
)

func Handle(request *IncomingRequest, out *ResponseOutparam) {
	path := request.PathWithQuery().SomeOr("/")
	if index := strings.IndexByte(path, '?'); index >= 0 {
		path = path[:index]
	}
	payload := []byte("go")
	if path == "/hello" {
		payload = []byte("hello")
	} else if path == "/language" {
		payload = []byte("go")
	} else if path == "/chain" {
		payload = chainPayload()
	} else if path == "/cpu" {
		var value uint64
		for i := uint64(0); i < 500000; i++ { value += i * 31 }
		payload = []byte(fmt.Sprintf("go:%d", value))
	} else if path == "/echo" {
		payload = readBody(request)
	}
	response := MakeOutgoingResponse(MakeFields())
	body := response.Body()
	ResponseOutparamSet(out, Ok[*OutgoingResponse, ErrorCode](response))
	if body.IsOk() {
		stream := body.Ok().Write()
		if stream.IsOk() {
			output := stream.Ok()
			output.BlockingWriteAndFlush(payload)
			output.Drop()
		}
		OutgoingBodyFinish(body.Ok(), None[*Fields]())
	}
}

func chainPayload() []byte {
	c, err := callLanguage("c-api")
	if err != nil {
		return []byte(`{"error":"c-api"}`)
	}
	javascript, err := callLanguage("javascript-api")
	if err != nil {
		return []byte(`{"error":"javascript-api"}`)
	}
	return []byte(fmt.Sprintf(`{"chain":["go","%s","%s"]}`, c, javascript))
}

func callLanguage(authority string) (string, error) {
	request := MakeOutgoingRequest(MakeFields())
	if result := request.SetScheme(Some(MakeSchemeHttp())); result.IsErr() {
		return "", fmt.Errorf("set scheme")
	}
	if result := request.SetAuthority(Some(authority)); result.IsErr() {
		return "", fmt.Errorf("set authority")
	}
	if result := request.SetPathWithQuery(Some("/language")); result.IsErr() {
		return "", fmt.Errorf("set path")
	}
	futureResult := wasi_http_outgoing_handler.Handle(request, None[*RequestOptions]())
	if futureResult.IsErr() {
		return "", fmt.Errorf("send: %v", futureResult.Err())
	}
	future := futureResult.Ok()
	pollable := future.Subscribe()
	pollable.Block()
	result := future.Get()
	pollable.Drop()
	future.Drop()
	if result.IsNone() {
		return "", fmt.Errorf("response not ready")
	}
	outer := result.Some()
	if outer.IsErr() {
		return "", fmt.Errorf("response: %v", outer.Err())
	}
	responseResult := outer.Ok()
	if responseResult.IsErr() {
		return "", fmt.Errorf("response: %v", responseResult.Err())
	}
	response := responseResult.Ok()
	bodyResult := response.Consume()
	if bodyResult.IsErr() {
		return "", fmt.Errorf("consume body")
	}
	body := bodyResult.Ok()
	streamResult := body.Stream()
	if streamResult.IsErr() {
		return "", fmt.Errorf("body stream")
	}
	stream := streamResult.Ok()
	var bytes []byte
	for {
		chunk := stream.BlockingRead(65536)
		if chunk.IsErr() || len(chunk.Ok()) == 0 {
			break
		}
		bytes = append(bytes, chunk.Ok()...)
	}
	stream.Drop()
	trailers := IncomingBodyFinish(body)
	trailers.Subscribe().Block()
	if result := trailers.Get(); result.IsSome() {
		if result.Some().IsOk() {
			fieldsResult := result.Some().Ok()
			if fieldsResult.IsOk() && fieldsResult.Ok().IsSome() {
				fieldsResult.Ok().Some().Drop()
			}
		}
	}
	return strings.TrimSpace(string(bytes)), nil
}

func readBody(request *IncomingRequest) []byte {
	body := request.Consume()
	if body.IsErr() { return nil }
	stream := body.Ok().Stream()
	if stream.IsErr() { return nil }
	var result []byte
	for {
		chunk := stream.Ok().BlockingRead(65536)
		if chunk.IsErr() || len(chunk.Ok()) == 0 { break }
		result = append(result, chunk.Ok()...)
	}
	return result
}
