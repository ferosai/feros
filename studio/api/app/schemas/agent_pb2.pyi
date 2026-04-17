from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class AudioLayout(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    AUDIO_LAYOUT_UNSPECIFIED: _ClassVar[AudioLayout]
    AUDIO_LAYOUT_STEREO: _ClassVar[AudioLayout]
    AUDIO_LAYOUT_MONO: _ClassVar[AudioLayout]

class AudioFormat(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    AUDIO_FORMAT_UNSPECIFIED: _ClassVar[AudioFormat]
    AUDIO_FORMAT_OPUS: _ClassVar[AudioFormat]
    AUDIO_FORMAT_WAV: _ClassVar[AudioFormat]
AUDIO_LAYOUT_UNSPECIFIED: AudioLayout
AUDIO_LAYOUT_STEREO: AudioLayout
AUDIO_LAYOUT_MONO: AudioLayout
AUDIO_FORMAT_UNSPECIFIED: AudioFormat
AUDIO_FORMAT_OPUS: AudioFormat
AUDIO_FORMAT_WAV: AudioFormat

class RecordingConfig(_message.Message):
    __slots__ = ("enabled", "output_uri", "audio_layout", "sample_rate", "audio_format", "max_duration_secs", "save_transcript", "include_tool_details", "include_llm_metadata")
    ENABLED_FIELD_NUMBER: _ClassVar[int]
    OUTPUT_URI_FIELD_NUMBER: _ClassVar[int]
    AUDIO_LAYOUT_FIELD_NUMBER: _ClassVar[int]
    SAMPLE_RATE_FIELD_NUMBER: _ClassVar[int]
    AUDIO_FORMAT_FIELD_NUMBER: _ClassVar[int]
    MAX_DURATION_SECS_FIELD_NUMBER: _ClassVar[int]
    SAVE_TRANSCRIPT_FIELD_NUMBER: _ClassVar[int]
    INCLUDE_TOOL_DETAILS_FIELD_NUMBER: _ClassVar[int]
    INCLUDE_LLM_METADATA_FIELD_NUMBER: _ClassVar[int]
    enabled: bool
    output_uri: str
    audio_layout: AudioLayout
    sample_rate: int
    audio_format: AudioFormat
    max_duration_secs: int
    save_transcript: bool
    include_tool_details: bool
    include_llm_metadata: bool
    def __init__(self, enabled: bool = ..., output_uri: _Optional[str] = ..., audio_layout: _Optional[_Union[AudioLayout, str]] = ..., sample_rate: _Optional[int] = ..., audio_format: _Optional[_Union[AudioFormat, str]] = ..., max_duration_secs: _Optional[int] = ..., save_transcript: bool = ..., include_tool_details: bool = ..., include_llm_metadata: bool = ...) -> None: ...

class ParamDef(_message.Message):
    __slots__ = ("name", "type", "description", "required", "options")
    NAME_FIELD_NUMBER: _ClassVar[int]
    TYPE_FIELD_NUMBER: _ClassVar[int]
    DESCRIPTION_FIELD_NUMBER: _ClassVar[int]
    REQUIRED_FIELD_NUMBER: _ClassVar[int]
    OPTIONS_FIELD_NUMBER: _ClassVar[int]
    name: str
    type: str
    description: str
    required: bool
    options: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, name: _Optional[str] = ..., type: _Optional[str] = ..., description: _Optional[str] = ..., required: bool = ..., options: _Optional[_Iterable[str]] = ...) -> None: ...

class ToolDef(_message.Message):
    __slots__ = ("description", "script", "params", "cancel_on_barge_in", "side_effect", "summarize_result")
    DESCRIPTION_FIELD_NUMBER: _ClassVar[int]
    SCRIPT_FIELD_NUMBER: _ClassVar[int]
    PARAMS_FIELD_NUMBER: _ClassVar[int]
    CANCEL_ON_BARGE_IN_FIELD_NUMBER: _ClassVar[int]
    SIDE_EFFECT_FIELD_NUMBER: _ClassVar[int]
    SUMMARIZE_RESULT_FIELD_NUMBER: _ClassVar[int]
    description: str
    script: str
    params: _containers.RepeatedCompositeFieldContainer[ParamDef]
    cancel_on_barge_in: bool
    side_effect: bool
    summarize_result: bool
    def __init__(self, description: _Optional[str] = ..., script: _Optional[str] = ..., params: _Optional[_Iterable[_Union[ParamDef, _Mapping]]] = ..., cancel_on_barge_in: bool = ..., side_effect: bool = ..., summarize_result: bool = ...) -> None: ...

class NodeDef(_message.Message):
    __slots__ = ("system_prompt", "tools", "edges", "model", "temperature", "max_tokens", "voice_id", "greeting")
    SYSTEM_PROMPT_FIELD_NUMBER: _ClassVar[int]
    TOOLS_FIELD_NUMBER: _ClassVar[int]
    EDGES_FIELD_NUMBER: _ClassVar[int]
    MODEL_FIELD_NUMBER: _ClassVar[int]
    TEMPERATURE_FIELD_NUMBER: _ClassVar[int]
    MAX_TOKENS_FIELD_NUMBER: _ClassVar[int]
    VOICE_ID_FIELD_NUMBER: _ClassVar[int]
    GREETING_FIELD_NUMBER: _ClassVar[int]
    system_prompt: str
    tools: _containers.RepeatedScalarFieldContainer[str]
    edges: _containers.RepeatedScalarFieldContainer[str]
    model: str
    temperature: float
    max_tokens: int
    voice_id: str
    greeting: str
    def __init__(self, system_prompt: _Optional[str] = ..., tools: _Optional[_Iterable[str]] = ..., edges: _Optional[_Iterable[str]] = ..., model: _Optional[str] = ..., temperature: _Optional[float] = ..., max_tokens: _Optional[int] = ..., voice_id: _Optional[str] = ..., greeting: _Optional[str] = ...) -> None: ...

class AgentGraphDef(_message.Message):
    __slots__ = ("entry", "nodes", "tools", "language", "timezone", "voice_id", "tts_provider", "tts_model", "recording", "config_schema_version", "gemini_live_model")
    class NodesEntry(_message.Message):
        __slots__ = ("key", "value")
        KEY_FIELD_NUMBER: _ClassVar[int]
        VALUE_FIELD_NUMBER: _ClassVar[int]
        key: str
        value: NodeDef
        def __init__(self, key: _Optional[str] = ..., value: _Optional[_Union[NodeDef, _Mapping]] = ...) -> None: ...
    class ToolsEntry(_message.Message):
        __slots__ = ("key", "value")
        KEY_FIELD_NUMBER: _ClassVar[int]
        VALUE_FIELD_NUMBER: _ClassVar[int]
        key: str
        value: ToolDef
        def __init__(self, key: _Optional[str] = ..., value: _Optional[_Union[ToolDef, _Mapping]] = ...) -> None: ...
    ENTRY_FIELD_NUMBER: _ClassVar[int]
    NODES_FIELD_NUMBER: _ClassVar[int]
    TOOLS_FIELD_NUMBER: _ClassVar[int]
    LANGUAGE_FIELD_NUMBER: _ClassVar[int]
    TIMEZONE_FIELD_NUMBER: _ClassVar[int]
    VOICE_ID_FIELD_NUMBER: _ClassVar[int]
    TTS_PROVIDER_FIELD_NUMBER: _ClassVar[int]
    TTS_MODEL_FIELD_NUMBER: _ClassVar[int]
    RECORDING_FIELD_NUMBER: _ClassVar[int]
    CONFIG_SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    GEMINI_LIVE_MODEL_FIELD_NUMBER: _ClassVar[int]
    entry: str
    nodes: _containers.MessageMap[str, NodeDef]
    tools: _containers.MessageMap[str, ToolDef]
    language: str
    timezone: str
    voice_id: str
    tts_provider: str
    tts_model: str
    recording: RecordingConfig
    config_schema_version: str
    gemini_live_model: str
    def __init__(self, entry: _Optional[str] = ..., nodes: _Optional[_Mapping[str, NodeDef]] = ..., tools: _Optional[_Mapping[str, ToolDef]] = ..., language: _Optional[str] = ..., timezone: _Optional[str] = ..., voice_id: _Optional[str] = ..., tts_provider: _Optional[str] = ..., tts_model: _Optional[str] = ..., recording: _Optional[_Union[RecordingConfig, _Mapping]] = ..., config_schema_version: _Optional[str] = ..., gemini_live_model: _Optional[str] = ...) -> None: ...
