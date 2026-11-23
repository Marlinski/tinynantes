//! The subset of the GTFS Realtime protobuf schema we read (trip updates), declared by hand with prost so the
//! build needs no protoc. Field numbers follow gtfs-realtime.proto.

#[derive(Clone, PartialEq, prost::Message)]
pub struct FeedMessage {
    #[prost(message, optional, tag = "1")]
    pub header: Option<FeedHeader>,
    #[prost(message, repeated, tag = "2")]
    pub entity: Vec<FeedEntity>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FeedHeader {
    #[prost(string, optional, tag = "1")]
    pub gtfs_realtime_version: Option<String>,
    #[prost(uint64, optional, tag = "3")]
    pub timestamp: Option<u64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FeedEntity {
    #[prost(string, optional, tag = "1")]
    pub id: Option<String>,
    #[prost(message, optional, tag = "3")]
    pub trip_update: Option<TripUpdate>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct TripUpdate {
    #[prost(message, optional, tag = "1")]
    pub trip: Option<TripDescriptor>,
    #[prost(message, repeated, tag = "2")]
    pub stop_time_update: Vec<StopTimeUpdate>,
    #[prost(uint64, optional, tag = "4")]
    pub timestamp: Option<u64>,
    #[prost(int32, optional, tag = "5")]
    pub delay: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct TripDescriptor {
    #[prost(string, optional, tag = "1")]
    pub trip_id: Option<String>,
    #[prost(string, optional, tag = "5")]
    pub route_id: Option<String>,
    #[prost(string, optional, tag = "3")]
    pub start_date: Option<String>,
    #[prost(int32, optional, tag = "4")]
    pub schedule_relationship: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct StopTimeUpdate {
    #[prost(uint32, optional, tag = "1")]
    pub stop_sequence: Option<u32>,
    #[prost(string, optional, tag = "4")]
    pub stop_id: Option<String>,
    #[prost(message, optional, tag = "2")]
    pub arrival: Option<StopTimeEvent>,
    #[prost(message, optional, tag = "3")]
    pub departure: Option<StopTimeEvent>,
    #[prost(int32, optional, tag = "5")]
    pub schedule_relationship: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct StopTimeEvent {
    #[prost(int32, optional, tag = "1")]
    pub delay: Option<i32>,
    #[prost(int64, optional, tag = "2")]
    pub time: Option<i64>,
}

pub const CANCELED: i32 = 3;
