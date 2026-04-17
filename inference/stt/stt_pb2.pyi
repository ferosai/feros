from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class SttRequest(_message.Message):
    __slots__ = ("audio", "finalize", "close")
    AUDIO_FIELD_NUMBER: _ClassVar[int]
    FINALIZE_FIELD_NUMBER: _ClassVar[int]
    CLOSE_FIELD_NUMBER: _ClassVar[int]
    audio: AudioData
    finalize: Finalize
    close: CloseStream
    def __init__(self, audio: _Optional[_Union[AudioData, _Mapping]] = ..., finalize: _Optional[_Union[Finalize, _Mapping]] = ..., close: _Optional[_Union[CloseStream, _Mapping]] = ...) -> None: ...

class AudioData(_message.Message):
    __slots__ = ("pcm_data",)
    PCM_DATA_FIELD_NUMBER: _ClassVar[int]
    pcm_data: bytes
    def __init__(self, pcm_data: _Optional[bytes] = ...) -> None: ...

class Finalize(_message.Message):
    __slots__ = ()
    def __init__(self) -> None: ...

class CloseStream(_message.Message):
    __slots__ = ()
    def __init__(self) -> None: ...

class SttResponse(_message.Message):
    __slots__ = ("transcript",)
    TRANSCRIPT_FIELD_NUMBER: _ClassVar[int]
    transcript: TranscriptResult
    def __init__(self, transcript: _Optional[_Union[TranscriptResult, _Mapping]] = ...) -> None: ...

class TranscriptResult(_message.Message):
    __slots__ = ("is_final", "text", "confidence", "start_time", "duration")
    IS_FINAL_FIELD_NUMBER: _ClassVar[int]
    TEXT_FIELD_NUMBER: _ClassVar[int]
    CONFIDENCE_FIELD_NUMBER: _ClassVar[int]
    START_TIME_FIELD_NUMBER: _ClassVar[int]
    DURATION_FIELD_NUMBER: _ClassVar[int]
    is_final: bool
    text: str
    confidence: float
    start_time: float
    duration: float
    def __init__(self, is_final: bool = ..., text: _Optional[str] = ..., confidence: _Optional[float] = ..., start_time: _Optional[float] = ..., duration: _Optional[float] = ...) -> None: ...
