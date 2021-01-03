use std::{collections::{HashSet, HashMap}, fs::File, io::{prelude::*, BufWriter}, iter::{FromIterator, from_fn}, time::Instant};

pub mod group;
pub mod footpath;
pub mod station;
pub mod trip;
pub mod algo;
mod path_finder;

use group::Group;

use petgraph::{EdgeDirection::{Incoming, Outgoing}, Graph, IntoWeightedEdge, dot::{Dot}, graph::{NodeIndex, EdgeIndex, DiGraph}};
use colored::*;


use crate::csv_reader;
use indexmap::IndexSet;

/// Node Type of the DiGraph
#[derive(Debug, Clone)]
pub enum NodeWeight {
    Departure { // departure of a train ride
        trip_id: u64,
        time: u64,
        station_id: String
    },

    Arrival { // arrival of a train ride
        trip_id: u64,
        time: u64,
        station_id: String
    },

    Transfer { // transfer node at a station, existing for every departure at that station
        time: u64,
        station_id: String
    },

    MainArrival {
        station_id: String
    },

    Default // empty default (used in intermediate subgraph)
}

impl Default for NodeWeight {
    fn default() -> Self { NodeWeight::Default }
}

impl NodeWeight {

    pub fn get_time(&self) -> Option<u64> {
        match self {
            Self::Departure {trip_id: _, time, station_id: _} => Some(*time),
            Self::Arrival {trip_id: _, time, station_id: _} => Some(*time),
            Self::Transfer {time, station_id: _} => Some(*time),
            _ => None
        }
    }

    pub fn get_station(&self) -> Option<String> {
        match self {
            Self::Departure {trip_id: _, time: _, station_id} => Some(station_id.clone()),
            Self::Transfer {time: _, station_id} => Some(station_id.clone()),
            _ => None
        }
    }

    pub fn is_arrival_at_station(&self, target_station_id: &str) -> bool {
        match self {
            Self::Arrival {trip_id: _, time: _, station_id} => station_id == target_station_id,
            _ => false
        }
    }
}

/// Edge Type of the DiGraph
#[derive(Debug, Clone)]
pub enum EdgeWeight {
    Ride { // edge between departure and arrival
        duration: u64,
        capacity: u64,
        utilization: u64,
    },

    WaitInTrain { // edge between arrival and departure in the same train (stay in the train)
        duration: u64
    },
    
    Board, // edge between transfer node and departure

    Alight { // edge between arrival and transfer
        duration: u64
    },

    WaitAtStation { // edge between two transfer nodes
        duration: u64
    },

    Walk { // edge between arrival and next transfer node at other station
        duration: u64
    },

    MainArrivalRelation // connects all arrivals to MainArrival node
}


impl EdgeWeight {

    // maps every edge to some virtual cost for improved DFS (aka. effort/expense to "take" the edge)
    pub fn cost(&self) -> u64 {
        match self {
            Self::Ride {duration: _, capacity: _, utilization: _} => 2,
            Self::WaitInTrain {duration: _} => 1,
            Self::Alight {duration: _} => 4,
            Self::WaitAtStation {duration: _} => 3,
            Self::Walk {duration: _} => 10,
            Self::Board => 5,
            Self::MainArrivalRelation => 0 // no cost, just a "meta" path
        }
    }


    /// is RideToStation Edge
    pub fn is_ride(&self) -> bool {
        match self {
            Self::Ride {
                duration: _, 
                capacity: _, 
                utilization: _
            } => true,
            _ => false,
        }
    }

    /// is Footpath Edge
    pub fn is_walk(&self) -> bool {
        match self {
            Self::Walk {
                duration: __
            } => true,
            _ => false,
        }
    }

    pub fn is_alight(&self) -> bool {
        match self {
            Self::Alight {
                duration: _
            } => true,
            _ => false
        }
    }

    /// get duration of self, defaults to 0
    pub fn get_duration(&self) -> u64 {
        match self {
            Self::Ride{duration, capacity: _, utilization: _} => *duration,
            Self::WaitInTrain{duration} => *duration,
            Self::Alight{duration} => *duration,
            Self::WaitAtStation{duration} => *duration,
            Self::Walk{duration} => *duration,
            _ => 0,
        }
    }

    /// get capacity of self, defaults to MAX
    pub fn get_capacity(&self) -> u64 {
        match self {
            Self::Ride{duration: _, capacity, utilization: _} => *capacity,
            _ => std::u64::MAX, // all other edges are not limited in terms of capacity
        }
    }

    /// increase utilization of this edge by <addend>
    pub fn increase_utilization(&mut self, addend: u64) {
        match self {
            Self::Ride{duration: _, capacity: _, utilization} => *utilization += addend,
            _ => {} // no need to track utilization on other edges, as they have unlimited capacity
        }
    }

    /// get utilization of self, defaults to 0
    pub fn get_utilization(&self) -> u64 {
        match self {
            Self::Ride{duration: _, capacity: _, utilization} => *utilization,
            _ => 0 // other edges always return 0 utilization as they have unlimited capacity
        }
    }

    pub fn get_remaining_capacity(&self) -> u64 {
        match self {
            Self::Ride{duration: _, capacity, utilization} => *capacity - *utilization,
            _ => u64::MAX // other edges always return u64::MAX as they have unlimited capacity
        }
    }
}

pub enum Object {
    Edge(EdgeWeight),
    Node(NodeWeight)
}

pub enum ObjectIndex {
    EdgeIndex(EdgeIndex),
    NodeIndex(NodeIndex),
}

/// entire combined data model
pub struct Model {
    pub graph: DiGraph<NodeWeight, EdgeWeight>,

    // we need to store all departure nodes for all stations at all times
    stations_departures: HashMap<String, Vec<(u64, NodeIndex)>>,
    station_arrival_main_node_indices: HashMap<String, NodeIndex>,

}

impl Model {

    pub fn with_stations_footpaths_and_trips(csv_folder_path: &str) -> Self {

        let footpath_maps = csv_reader::read_to_maps(&format!("{}footpaths.csv", csv_folder_path));
        let station_maps = csv_reader::read_to_maps(&format!("{}stations.csv", csv_folder_path));
        let trip_maps = csv_reader::read_to_maps(&format!("{}trips.csv", csv_folder_path));

        // convert each list of maps into a single map with multiple entries with id as key
        let footpaths_vec = footpath::Footpath::from_maps_to_vec(&footpath_maps);
        let mut stations_map = station::Station::from_maps_to_map(&station_maps);
        let trips_map = trip::Trip::from_maps_to_map(&trip_maps);

        let mut graph = DiGraph::new();
        let mut stations_departures = HashMap::with_capacity(stations_map.len());
        let mut station_arrival_main_node_indices = HashMap::with_capacity(stations_map.len());

        // iterate over all trips
        for (_, trip) in trips_map.iter() {

            // ARRIVAL NODE
            let arrival_node_index = graph.add_node(NodeWeight::Arrival {
                trip_id: trip.id,
                time: trip.arrival,
                station_id: trip.to_station.clone()
            });

            // DEPARTURE NODE
            let departure_node_index = graph.add_node(NodeWeight::Departure {
                trip_id: trip.id,
                time: trip.departure,
                station_id: trip.from_station.clone()
            });

            // add these nodes to a station
            let to_station = stations_map.get_mut(&trip.to_station).unwrap();
            to_station.arrival_node_indices.insert(trip.id, arrival_node_index);

            let from_station = stations_map.get_mut(&trip.from_station).unwrap();
            from_station.departure_node_indices.insert(trip.id, departure_node_index);

            // connect stations of this trip
            graph.add_edge(departure_node_index, arrival_node_index, EdgeWeight::Ride {
                capacity: trip.capacity,
                duration: trip.arrival - trip.departure,
                utilization: 0
            });
        }

        // iterate over all stations (first run only inserts departures and transfers)
        for (station_id, station) in stations_map.iter_mut() {

            // iterate over all departures
            for (trip_id, departure_node_index) in station.departure_node_indices.iter() {

                let departure_node = graph.node_weight(*departure_node_index).unwrap();
                let departure_node_time = departure_node.get_time().unwrap();

                // DEPARTURE TRANSFER NODE (each departure also induces a corresponding departure node at the station)
                let departure_transfer_node_index = graph.add_node(NodeWeight::Transfer {
                    time: departure_node_time,
                    station_id: station_id.clone()
                });
                // edge between transfer of this station to departure
                graph.add_edge(departure_transfer_node_index, *departure_node_index, EdgeWeight::Board);

                // add transfer node to list of transfer nodes of this station
                station.transfer_node_indices.push((departure_node_time, departure_transfer_node_index));

                // connect arrival of this trip to departure of this trip (if exists)
                // this edge represents staying in the same train
                match station.arrival_node_indices.get(trip_id) {
                    Some(arrival_node_index) => {
                        let arrival_node_time = graph.node_weight(*arrival_node_index).unwrap().get_time().unwrap();

                        graph.add_edge(*arrival_node_index, *departure_node_index, EdgeWeight::WaitInTrain {
                            duration: departure_node_time - arrival_node_time
                        });
                    },
                    None => {}
                }
            }

            // sort transfer node list by time (first tuple element)
            station.transfer_node_indices.sort_unstable_by_key(|(time, _)| *time);

            // connect transfers with each other
            for transfer_node_indices in station.transfer_node_indices.windows(2) {
                graph.add_edge(transfer_node_indices[0].1, transfer_node_indices[1].1, EdgeWeight::WaitAtStation {
                    duration: transfer_node_indices[1].0 - transfer_node_indices[0].0
                });
            }
        }

        // iterate over all stations (second run to add arrivals and connect them to transfers)
        for (station_id, station) in stations_map.iter_mut() {

            let station_arrival_main_node_index = graph.add_node(NodeWeight::MainArrival {
                station_id: station_id.clone()
            });

            // iterate over all arrivals
            for (_, arrival_node_index) in station.arrival_node_indices.iter() {

                // connect arrival to station's main node
                graph.add_edge(*arrival_node_index, station_arrival_main_node_index, EdgeWeight::MainArrivalRelation);

                let arrival_node = graph.node_weight(*arrival_node_index).expect("Could not find node in graph");
                let arrival_node_time = arrival_node.get_time().unwrap();

                let earliest_transfer_time = arrival_node_time + station.transfer_time;

                // try to find next transfer node at this station
                for (transfer_timestamp, transfer_node_index) in station.transfer_node_indices.iter() {
                    if earliest_transfer_time <= *transfer_timestamp {
                        graph.add_edge(*arrival_node_index, *transfer_node_index, EdgeWeight::Alight {
                            duration: station.transfer_time
                        });
                        break // the loop
                    }
                }
            }

            // save references to all transfers and to arrival_main
            stations_departures.insert(station_id.clone(), station.transfer_node_indices.clone());
            station_arrival_main_node_indices.insert(station_id.clone(), station_arrival_main_node_index);
        }



        let mut successful_footpath_counter: u64 = 0;
        let mut failed_footpath_counter: u64 = 0;

        // iterate over all footpaths
        for footpath in footpaths_vec.iter() {
            let from_station = match stations_map.get(&footpath.from_station) {
                Some(from_station) => from_station,
                None => {
                    println!("footpath's from_station {} unknown", footpath.from_station);
                    continue // with next footpath
                }
            };

            let to_station = match stations_map.get(&footpath.to_station) {
                Some(to_station) => to_station,
                None => {
                    println!("footpath's to_station {} unknown", footpath.to_station);
                    continue // with next footpath
                }
            };


            // for every arrival at the from_station try to find the next transfer node at the to_station
            for arrival_node_index in from_station.arrival_node_indices.values() {
                let arrival_node_time = graph.node_weight(*arrival_node_index).unwrap().get_time().unwrap();

                // timestamp of arrival at the footpaths to_station
                let earliest_transfer_time = arrival_node_time + footpath.duration;

                let mut edge_added = false;

                // try to find next transfer node at to_station (requires transfer_node_indices to be sorted, earliest first)
                for (transfer_timestamp, transfer_node_index) in to_station.transfer_node_indices.iter() {
                    if earliest_transfer_time <= *transfer_timestamp {
                        graph.add_edge(*arrival_node_index, *transfer_node_index, EdgeWeight::Walk {
                            duration: footpath.duration
                        });
                        edge_added = true;
                        successful_footpath_counter += 1;
                        break // the inner loop
                    }
                }

                if !edge_added {
                    failed_footpath_counter += 1;
                    //println!("There couldn't be found any valid (time) transfer node for footpath from {} -> {}", footpath.from_station, footpath.to_station);
                }
            }
        }

        println!("successful_footpaths: {}, failed_footpaths: {}", successful_footpath_counter, failed_footpath_counter);

        println!("node_count={}, edge_count={}", graph.node_count(), graph.edge_count());

        Self {
            graph,
            stations_departures,
            station_arrival_main_node_indices
        }
    }

    pub fn to_dot(&self) -> String {
        format!("{:?}", Dot::with_config(&self.graph, &[]))
    }


    pub fn find_solutions(&mut self, groups_csv_filepath: &str) {
        // Bei den Reisendengruppen gibt es noch eine Änderung: Eine zusätzliche Spalte "in_trip" gibt jetzt an, in welchem Trip sich die Gruppe aktuell befindet. Die Spalte kann entweder leer sein (dann befindet sich die Gruppe aktuell in keinem Trip, sondern an der angegebenen Station) oder eine Trip ID angeben (dann befindet sich die Gruppe aktuell in diesem Trip und kann frühestens an der angegebenen Station aussteigen).
        // Das beeinflusst den Quellknoten der Gruppe beim MCFP: Befindet sich die Gruppe in einem Trip sollte der Quellknoten der entsprechende Ankunftsknoten (oder ein zusätzlich eingefügter Hilfsknoten, der mit diesem verbunden ist) sein. Befindet sich die Gruppe an einer Station, sollte der Quellknoten ein Warteknoten an der Station (oder ein zusätzlich eingefügter Hilfsknoten, der mit diesem verbunden ist) sein.
        // Falls die Gruppe an einer Station startet, muss in diesem Fall am Anfang die Stationsumstiegszeit berücksichtigt werden (kann man sich so vorstellen: die Gruppe steht irgendwo an der Station und muss erst zu dem richtigen Gleis laufen).
        // Befindet sich die Gruppe hingegen in einem Trip, hat sie zusätzlich die Möglichkeit, mit diesem weiterzufahren und erst später umzusteigen. (Würde man sie an der Station starten lassen, wäre die Stationsumstiegszeit nötig, um wieder in den Trip einzusteigen, in dem sie eigentlich schon ist - und meistens ist die Standzeit des Trips geringer als die Stationsumstiegszeit)
        // Habe auch die Formatbeschreibung im handcrafted-scenarios Repo entsprechend angepasst.


        let group_maps = csv_reader::read_to_maps(groups_csv_filepath);
        let groups_map = Group::from_maps_to_map(&group_maps);
        let mut subgraph: DiGraph<NodeWeight, EdgeWeight> = Graph::new();
        let mut node_index_graph_subgraph_mapping: HashMap<NodeIndex, NodeIndex> = HashMap::new();

        let mut groups_sorted: Vec<&Group> = groups_map.values().collect();
        groups_sorted.sort_unstable_by_key(|group| group.passengers);
        groups_sorted.reverse();

        for group_value in groups_sorted.into_iter(){

            let from_node_index = self.find_start_node_index(&group_value.start, group_value.departure).expect("Could not find departure at from_station");
            let to_node_index = self.find_end_node_index(&group_value.destination).expect("Could not find destination station");

            // max duration should depend on the original travel time
            let travel_time = group_value.arrival - group_value.departure;
            let max_duration = (travel_time as f64 * 2.0) as u64; // todo: factor to modify later if not a path could be found for all groups

            let start = Instant::now();
            print!("[group={}]: {} -> {} with {} passenger(s) in {} min(s) ... ", group_value.id, group_value.start, group_value.destination, group_value.passengers, max_duration);

            let mut paths_recursive = path_finder::all_paths_dfs_recursive(
                &self.graph, 
                from_node_index, 
                to_node_index, //|node| node.is_arrival_at_station(&group_value.destination), // dynamic condition for dfs algorithm to find arrival node

                group_value.passengers as u64, 
                max_duration, 
                100 // initial budget for cost (each edge has individual search cost)
            );

            print!("done in {}ms ... ", start.elapsed().as_millis());

            // sort paths by remaining duration (highest first)
            paths_recursive.sort_unstable_by_key(|(remaining_duration, _)| *remaining_duration);
            paths_recursive.reverse();

            let output = match paths_recursive.first() {
                Some((remaining_duration, path)) => {

                    for edge_index in path.iter() {
                        
                        self.graph.edge_weight_mut(*edge_index).unwrap().increase_utilization(group_value.passengers as u64);
                    }

                    format!("augmenting best path (remaining_duration={}, len={})", remaining_duration, path.len()).green()
                },

                None => {
                    "no path to augment".red()
                }
            };

            println!("{}", output);
            
            //let paths_recursive = self.all_simple_paths_dfs_dorian(from_node_index, to_node_index, max_duration, 5);

            
            

            // let all_edges_in_paths_recursive: HashSet<EdgeIndex> = paths_recursive.iter().flatten().cloned().collect();
            // if all_edges_in_paths_recursive.len() > 0 {
            //     let subgraph = self.build_subgraph_with_edges(&all_edges_in_paths_recursive);

            //     BufWriter::new(File::create(format!("graphs/groups/{}.dot", group_value.id)).unwrap()).write(
            //         format!("{:?}", Dot::with_config(&subgraph, &[])).as_bytes()
            //     ).unwrap();
            // }

            // let subgraph_paths = self.create_subgraph_with_nodes(&mut subgraph, paths_recursive, &mut node_index_graph_subgraph_mapping);
    
            // let dot_code = format!("{:?}", Dot::with_config(&subgraph, &[]));
    
            // BufWriter::new(File::create(format!("graphs/subgraph_group_{}.dot", group_key)).unwrap()).write(
            //     dot_code.as_bytes()
            // ).unwrap();
        }

        // let dot_code = format!("{:?}", Dot::with_config(&subgraph, &[]));
    
        // BufWriter::new(File::create(format!("graphs/subgraph_complete.dot")).unwrap()).write(
        //     dot_code.as_bytes()
        // ).unwrap();


        // todo: iterate groups, augment routes ... return solutions
    }


    pub fn find_start_node_index(&self, station_id: &str, start_time: u64) -> Option<NodeIndex> {
        match self.stations_departures.get(station_id) {
            Some(station_departures) => {
                
                // iterate until we find a departure time >= the time we want to start
                for (departure_time, departure_node_index) in station_departures.iter() {
                    if start_time <= *departure_time {
                        return Some(*departure_node_index);
                    }
                }

                // no departure >= start_time found
                None
            },

            // station not found
            None => None
        }
    }


    pub fn find_end_node_index(&self, station_id: &str) -> Option<NodeIndex> {
        match self.station_arrival_main_node_indices.get(station_id) {
            Some(station_arrival_main_node_index) => Some(*station_arrival_main_node_index),
            None => None
        }
    }


    /// builds subgraph that only contains nodes connected by edges
    pub fn build_subgraph_with_edges(&self, edges: &HashSet<EdgeIndex>) -> DiGraph<NodeWeight, EdgeWeight> {

        self.graph.filter_map(
            |node_index, node_weight| {

                // check if at least one incoming/outgoing edge of current node is in HashSet of edges
                let mut walker = self.graph.neighbors_undirected(node_index).detach();
                while let Some((current_edge, _)) = walker.next(&self.graph) {
                    if edges.contains(&current_edge) {
                        return Some(node_weight.clone());
                    }
                }

                // no edge in set -> do not include node in graph
                None
            },
            |edge_index, edge_weight| {
                if edges.contains(&edge_index) {
                    Some(edge_weight.clone())
                } else {
                    None
                }
            }
        )
    }



    fn all_simple_paths_dfs_dorian(&self, from_node_index: NodeIndex, to_node_index: NodeIndex, max_duration: u64, max_rides: u64) -> Vec<Vec<NodeIndex>> {

        // list of already visited nodes
        let mut visited: IndexSet<NodeIndex> = IndexSet::from_iter(Some(from_node_index));

        // list of childs of currently exploring path nodes,
        // last elem is list of childs of last visited node
        let mut stack = vec![self.graph.neighbors_directed(from_node_index, Outgoing).detach()];
        let mut durations: Vec<u64> = vec![0];
        let mut rides: Vec<u64> = vec![0];

        //let mut bfs = self.graph.neighbors_directed(from_node_index, Outgoing).detach();
        //let mut a = bfs.next(&self.graph);
        let path_finder = from_fn(move || {
            while let Some(children) = stack.last_mut() {
                if let Some((child_edge_index, child_node_index)) = children.next(&self.graph) {
                    if durations.iter().sum::<u64>() < max_duration && rides.iter().sum::<u64>() < max_rides {
                        if child_node_index == to_node_index {
                            let path = visited
                                .iter()
                                .cloned()
                                .chain(Some(child_node_index))
                                .collect::<Vec<NodeIndex>>();
                            return Some(path);
                        } else if !visited.contains(&child_node_index) {
                            let edge_weight = self.graph.edge_weight(child_edge_index).unwrap();
                            durations.push(edge_weight.get_duration());
                            // only count ride to station and walk to station as limit factor
                            if edge_weight.is_ride() || edge_weight.is_walk() {
                                rides.push(1);
                            } else {
                                rides.push(0);
                            };
                            //rides.push(1);
                            visited.insert(child_node_index);
                            stack.push(self.graph.neighbors_directed(child_node_index, Outgoing).detach());
                        }
                    } else {
                        let mut children_any_to_node_index = false;
                        while let Some((_, c_node_index)) = children.next(&self.graph) {
                            if c_node_index == to_node_index {
                                children_any_to_node_index = true;
                                break;
                            }
                        }
                        if child_node_index == to_node_index || children_any_to_node_index { //} || children..  any(|v| v == to_node_index) {
                            let path = visited
                                .iter()
                                .cloned()
                                .chain(Some(to_node_index))
                                .collect::<Vec<NodeIndex>>();
                            return Some(path);
                        } 
                        stack.pop();
                        visited.pop();
                        durations.pop();
                        rides.pop();
                    }
                    
                } else {
                    stack.pop();
                    visited.pop();
                    durations.pop();
                    rides.pop();
                }
            }
            
            None
        });
    
        path_finder.collect::<Vec<_>>()
    }


    // creates a subgraph of self with only the part of the graph of specified paths
    pub fn create_subgraph_with_nodes(&self, subgraph: &mut Graph<NodeWeight, EdgeWeight>, paths: Vec<Vec<NodeIndex>>, node_index_graph_subgraph_mapping: &mut HashMap<NodeIndex, NodeIndex>) -> Vec<Vec<ObjectIndex>> {
        //let mut subgraph = DiGraph::new();
        let mut subgraph_paths: Vec<Vec<ObjectIndex>> = Vec::new();

        // iterate all paths in graph
        for path in paths {

            let mut subgraph_path_indices: Vec<ObjectIndex> = Vec::new();
            let mut path_max_flow: u64 = std::u64::MAX;
            let mut path_edge_indices: Vec<EdgeIndex> = Vec::new();

            // iterate over all NodeIndex pairs in this path
            for graph_node_index_pair in path.windows(2) {

                // check if the first node already exists in subgraph
                let subgraph_node_a_index = match node_index_graph_subgraph_mapping.get(&graph_node_index_pair[0]) {
                    Some(subgraph_node_index) => *subgraph_node_index,
                    None => {
                        // clone NodeWeight from graph
                        let node_weight = self.graph.node_weight(graph_node_index_pair[0]).unwrap().clone();

                        // create new node in subgraph
                        let subgraph_node_index = subgraph.add_node(node_weight);
                        
                        // insert mapping into HashMap
                        node_index_graph_subgraph_mapping.insert(graph_node_index_pair[0], subgraph_node_index.clone());

                        subgraph_node_index
                    }
                };
                    
                // check if the second node already exists in subgraph
                let subgraph_node_b_index = match node_index_graph_subgraph_mapping.get(&graph_node_index_pair[1]) {
                    Some(subgraph_node_index) => *subgraph_node_index,
                    None => {
                        // clone NodeWeight from graph
                        let node_weight = self.graph.node_weight(graph_node_index_pair[1]).unwrap().clone();

                        // create new node in subgraph
                        let subgraph_node_index = subgraph.add_node(node_weight);
                        
                        // insert mapping into HashMap
                        node_index_graph_subgraph_mapping.insert(graph_node_index_pair[1], subgraph_node_index);

                        subgraph_node_index
                    }
                };
                
                // add outgoing node to path if path is empty
                if subgraph_path_indices.is_empty() {
                    subgraph_path_indices.push(ObjectIndex::NodeIndex(subgraph_node_a_index));
                };

                // create edge if there was created at least one new node
                let subgraph_edge_weight = match subgraph.find_edge(subgraph_node_a_index, subgraph_node_b_index) {
                    Some(subgraph_edge_index) => {
                        // add edge to path
                        subgraph_path_indices.push(ObjectIndex::EdgeIndex(subgraph_edge_index));
                        path_edge_indices.push(subgraph_edge_index);
                        subgraph.edge_weight(subgraph_edge_index).unwrap()
                    },
                    None => {
                        let graph_edge_index = self.graph.find_edge(graph_node_index_pair[0], graph_node_index_pair[1]).unwrap();
                        let subgraph_edge_weight = self.graph.edge_weight(graph_edge_index).unwrap().clone();

                        let subgraph_edge_index = subgraph.add_edge(subgraph_node_a_index, subgraph_node_b_index, subgraph_edge_weight);
                        // add edge to path
                        subgraph_path_indices.push(ObjectIndex::EdgeIndex(subgraph_edge_index));
                        path_edge_indices.push(subgraph_edge_index);
                        subgraph.edge_weight(subgraph_edge_index).unwrap()
                    }
                };

                // update max_flow if edge capacity is smaller current path_max_flow
                let edge_remaining_flow = subgraph_edge_weight.get_capacity() - subgraph_edge_weight.get_utilization();
                if edge_remaining_flow < path_max_flow {
                    path_max_flow = edge_remaining_flow;
                };
                
                subgraph_path_indices.push(ObjectIndex::NodeIndex(subgraph_node_b_index));
            };

            subgraph_paths.push(subgraph_path_indices);

            // set utilization to all edges of path
            for path_edge_index in path_edge_indices {
                subgraph.edge_weight_mut(path_edge_index).unwrap().increase_utilization(path_max_flow);
                //println!("{}, {}", path_max_flow, subgraph.edge_weight(path_edge_index).unwrap().get_utilization())
            }
        }

        subgraph_paths
    }




}
